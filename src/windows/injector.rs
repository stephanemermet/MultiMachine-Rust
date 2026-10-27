//! SendInput never runs on the low-level hook thread. Reset releases keys actually
//! submitted by the worker, including a DOWN that was in flight when Reset arrived.
use super::platform::*;
use multimachine_rust::{engine::Barrier, model::*};
use parking_lot::{Condvar, Mutex};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
#[derive(Clone)]
pub struct Injector(Arc<Shared>);
enum Item {
    Input(Input),
    Barrier(u64, Barrier),
    Reset,
    SourceUp,
}
struct Queue {
    items: VecDeque<Item>,
    stop: bool,
}
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    join: Mutex<Option<thread::JoinHandle<()>>>,
    desktop_available: AtomicBool,
}
impl Injector {
    pub fn new(done: Arc<dyn Fn(u64, Barrier) + Send + Sync>, error: Notice) -> Self {
        let s = Arc::new(Shared {
            queue: Mutex::new(Queue {
                items: VecDeque::new(),
                stop: false,
            }),
            ready: Condvar::new(),
            join: Mutex::new(None),
            desktop_available: AtomicBool::new(true),
        });
        let shared = s.clone();
        let worker = thread::Builder::new()
            .name("MMR native input".into())
            .spawn(move || {
                let mut held = HashMap::<u16, Input>::new();
                loop {
                    let item = {
                        let mut q = shared.queue.lock();
                        while q.items.is_empty() && !q.stop {
                            shared.ready.wait(&mut q);
                        }
                        if q.items.is_empty() && q.stop {
                            break;
                        }
                        q.items.pop_front().unwrap()
                    };
                    match item {
                        Item::Input(input) => {
                            if !shared.desktop_available.load(Ordering::Acquire) {
                                continue;
                            }
                            if !send(&input) {
                                error(
                                    "Windows a refusé une entrée. Le contrôle est arrêté.".into(),
                                );
                                release(&mut held);
                                continue;
                            }
                            if let Some((key, up)) = key(&input) {
                                if up {
                                    held.remove(&key);
                                } else {
                                    held.insert(key, input);
                                }
                            }
                        }
                        Item::Barrier(epoch, kind) => done(epoch, kind),
                        Item::Reset => {
                            if shared.desktop_available.load(Ordering::Acquire) {
                                release(&mut held);
                            }
                        }
                        Item::SourceUp => {
                            let released = shared.desktop_available.load(Ordering::Acquire)
                                && send(&Input::Button {
                                    button: 1,
                                    up: true,
                                });
                            if released {
                                held.remove(&1);
                            } else {
                                held.insert(
                                    1,
                                    Input::Button {
                                        button: 1,
                                        up: false,
                                    },
                                );
                            }
                        }
                    }
                }
                release(&mut held);
            })
            .expect("input worker");
        *s.join.lock() = Some(worker);
        Self(s)
    }
    pub fn input(&self, input: Input) -> bool {
        let mut q = self.0.queue.lock();
        if q.stop {
            return false;
        }
        if matches!(input, Input::Move { .. })
            && matches!(q.items.back(), Some(Item::Input(Input::Move { .. })))
        {
            *q.items.back_mut().unwrap() = Item::Input(input);
            return true;
        }
        if q.items.len() >= 512 {
            q.items.clear();
            q.items.push_back(Item::Reset);
            self.0.ready.notify_one();
            return false;
        }
        q.items.push_back(Item::Input(input));
        self.0.ready.notify_one();
        true
    }
    pub fn barrier(&self, epoch: u64, kind: Barrier) {
        let mut q = self.0.queue.lock();
        if !q.stop {
            q.items.push_back(Item::Barrier(epoch, kind));
            self.0.ready.notify_one();
        }
    }
    pub fn reset(&self) {
        let mut q = self.0.queue.lock();
        q.items.clear();
        q.items.push_back(Item::Reset);
        self.0.ready.notify_one();
    }
    pub fn desktop_available(&self, available: bool) {
        self.0.desktop_available.store(available, Ordering::Release);
        self.reset();
    }
    pub fn source_up(&self) {
        let mut q = self.0.queue.lock();
        q.items.push_back(Item::SourceUp);
        self.0.ready.notify_one();
    }
    pub fn stop(&self) {
        self.reset();
        self.0.queue.lock().stop = true;
        self.0.ready.notify_one();
        let worker = self.0.join.lock().take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}
fn key(input: &Input) -> Option<(u16, bool)> {
    match input {
        Input::Key { key, up, .. } => Some((key + 256, *up)),
        Input::Button { button, up } => Some((*button as u16, *up)),
        _ => None,
    }
}
fn release(held: &mut HashMap<u16, Input>) {
    // An UP refused during a desktop switch is still owed. Retain it for the
    // reset performed on return, instead of losing the evidence of our DOWN.
    held.retain(|_, input| input.release().is_some_and(|up| !send(&up)));
}
fn send(input: &Input) -> bool {
    unsafe {
        let native = match *input {
            Input::Move { position } => {
                let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
                let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
                let w = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2);
                let h = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2);
                INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            dx: ((position.x - x) as i64 * 65535 / (w - 1) as i64) as i32,
                            dy: ((position.y - y) as i64 * 65535 / (h - 1) as i64) as i32,
                            dwFlags: MOUSEEVENTF_MOVE
                                | MOUSEEVENTF_ABSOLUTE
                                | MOUSEEVENTF_VIRTUALDESK,
                            dwExtraInfo: INPUT_TAG,
                            ..Default::default()
                        },
                    },
                }
            }
            Input::Button { button, up } => {
                let flags = match (button, up) {
                    (1, false) => MOUSEEVENTF_LEFTDOWN,
                    (1, true) => MOUSEEVENTF_LEFTUP,
                    (2, false) => MOUSEEVENTF_RIGHTDOWN,
                    (2, true) => MOUSEEVENTF_RIGHTUP,
                    (3, false) => MOUSEEVENTF_MIDDLEDOWN,
                    (3, true) => MOUSEEVENTF_MIDDLEUP,
                    (_, false) => MOUSEEVENTF_XDOWN,
                    (_, true) => MOUSEEVENTF_XUP,
                };
                INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            dwFlags: flags,
                            mouseData: if button > 3 { (button - 3) as u32 } else { 0 },
                            dwExtraInfo: INPUT_TAG,
                            ..Default::default()
                        },
                    },
                }
            }
            Input::Wheel { horizontal, delta } => INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dwFlags: if horizontal {
                            MOUSEEVENTF_HWHEEL
                        } else {
                            MOUSEEVENTF_WHEEL
                        },
                        mouseData: delta as i32 as u32,
                        dwExtraInfo: INPUT_TAG,
                        ..Default::default()
                    },
                },
            },
            Input::Key {
                key,
                scan,
                extended,
                up,
            } => {
                let scanned = scan != 0 && key != 0x13;
                INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VIRTUAL_KEY(if scanned { 0 } else { key }),
                            wScan: scan,
                            dwFlags: KEYBD_EVENT_FLAGS(
                                (if scanned { 8 } else { 0 })
                                    | (if up { 2 } else { 0 })
                                    | (if extended { 1 } else { 0 }),
                            ),
                            dwExtraInfo: INPUT_TAG,
                            ..Default::default()
                        },
                    },
                }
            }
        };
        SendInput(&[native], std::mem::size_of::<INPUT>() as i32) == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_desktop_discards_inputs_and_releases_previous_down_on_resume() {
        let (tx, rx) = std::sync::mpsc::channel();
        let i = Injector::new(
            Arc::new(move |epoch, _| {
                let _ = tx.send(epoch);
            }),
            Arc::new(|_| {}),
        );
        let flush = |epoch| {
            i.barrier(epoch, Barrier::Begin(uuid::Uuid::new_v4()));
            assert_eq!(
                rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
                epoch
            );
        };
        i.input(Input::Button {
            button: 1,
            up: false,
        });
        flush(1);
        assert!(unsafe { GetAsyncKeyState(1) } < 0);
        i.desktop_available(false);
        i.input(Input::Button {
            button: 1,
            up: true,
        });
        i.input(Input::Button {
            button: 2,
            up: false,
        });
        flush(2);
        assert!(
            unsafe { GetAsyncKeyState(1) } < 0,
            "owed release must survive interruption"
        );
        assert!(
            unsafe { GetAsyncKeyState(2) } >= 0,
            "new input must be discarded"
        );
        i.desktop_available(true);
        flush(3);
        assert!(
            unsafe { GetAsyncKeyState(1) } >= 0,
            "resume must release prior DOWN"
        );
        i.input(Input::Button {
            button: 1,
            up: false,
        });
        flush(4);
        assert!(
            unsafe { GetAsyncKeyState(1) } < 0,
            "new control works without relaunch"
        );
        i.stop();
        assert!(unsafe { GetAsyncKeyState(1) } >= 0);
    }
    #[test]
    fn reset_and_shutdown_balance_native_button_down() {
        let (tx, rx) = std::sync::mpsc::channel();
        let done = Arc::new(move |epoch, kind| {
            let _ = tx.send((epoch, kind));
        });
        let i = Injector::new(done, Arc::new(|_| {}));
        let gesture = uuid::Uuid::new_v4();
        for _ in 0..50 {
            assert!(i.input(Input::Button {
                button: 1,
                up: false
            }));
            i.reset();
            i.barrier(1, Barrier::Finish(gesture));
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            assert!(unsafe { GetAsyncKeyState(1) } >= 0);
        }
        assert!(i.input(Input::Button {
            button: 1,
            up: false
        }));
        i.stop();
        assert!(unsafe { GetAsyncKeyState(1) } >= 0);
    }
}
