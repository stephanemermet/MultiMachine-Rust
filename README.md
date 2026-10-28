# MultiMachine Rust

Réécriture indépendante de MultiMachine pour **deux PC Windows 10/11 x64 sur le LAN**. L’application et le protocole sont écrits en Rust ; l’interface, les hooks et le glisser-déposer utilisent Win32/COM. Aucun runtime .NET, installateur ou droit administrateur n’est nécessaire à l’application.

**Livraison Rust : préversion 0.1.1.** Les tests automatisés Linux et Windows sous Wine sont décrits dans [VALIDATION.md](docs/VALIDATION.md). Ils ne constituent pas une recette sur deux Windows physiques. Le projet .NET reste indépendant et intact. Les deux produits ne peuvent pas communiquer entre eux. Installer la même version Rust sur les deux PC pour bénéficier du fonctionnement au repos des deux côtés.

## Utilisation

1. Extraire le ZIP dans un dossier accessible en écriture, puis lancer `MultiMachineRust.exe` sur chaque PC. Fermer la version .NET pendant l’essai pour qu’un seul logiciel intercepte les périphériques.
2. Sur le PC pouvant accepter une connexion entrante, choisir **Serveur**, puis **Démarrer**. Sur le PC soumis aux restrictions corporate, choisir **Client**.
3. Côté client, laisser l’adresse vide pour la découverte automatique, ou cliquer **Détecter**, ou saisir l’IP du serveur. Cliquer **Démarrer**.
4. À la première association, comparer le code affiché sur le client à celui de la demande sur le serveur. Confirmer sur le serveur. L’identité est ensuite mémorisée ; aucune chaîne à transporter entre machines.
5. Ouvrir **Écrans…** une fois connectés : placer l’autre PC à gauche ou à droite et ajuster la hauteur. La disposition interne des écrans de chaque PC reste celle des réglages d’affichage Windows.
6. Franchir le bord commun. Les périphériques physiques de l’un ou l’autre PC peuvent contrôler l’autre sans changer les rôles réseau. **Ctrl + Alt + F12** reprend la main. L’activité physique sur le PC recevant le contrôle interrompt le contrôle distant.

La fenêtre se redimensionne et adapte ses options à la largeur et au DPI du moniteur. Une barre de défilement permet d’accéder au bas du formulaire si l’écran est trop petit. L’icône est intégrée au programme, à sa fenêtre et à la zone de notification.

Le halo bleu apparaît aux deux extrémités du passage. L’option **Traces du curseur distant** active les traces Windows pendant la réception du contrôle et restaure le réglage antérieur au retour. Un journal permet la restauration au prochain lancement après un arrêt brutal.

**Mouvement hybride UDP + TCP** est activé par défaut. Le décocher relance la connexion en TCP classique. Les clics, touches et défilements restent fiables ; les positions intermédiaires peuvent être remplacées. L’absence de réponse UDP déclenche le transport TCP de secours. La découverte utilise UDP 24883 ; le contrôle TLS et les mouvements UDP utilisent le même numéro de port, 24882 par défaut. Les transferts disposent d’une seconde connexion TLS. Le pare-feu du serveur doit autoriser les connexions locales au port choisi ; les politiques corporate ne sont pas contournées.

Après deux secondes sans activité utile, les sondes UDP s’arrêtent et la surveillance TCP s’espace à 30 secondes. Une nouvelle opération réactive immédiatement la surveillance ; la première position peut utiliser TCP pendant la reprise UDP. Une touche ou un bouton maintenu, ou un glisser-déposer en cours, conserve une détection rapide des coupures. Le « ping TCP aller-retour » affiché pendant l’activité mesure la liaison et son traitement, pas la latence aller simple du curseur UDP. Aucun rafraîchissement de statistiques n’est effectué dans la fenêtre masquée ; aucun journal périodique de trafic n’est écrit. **Diagnostics** consigne les compteurs uniquement à la demande.

Les options de démarrage et de connexion automatiques sont indépendantes. Le serveur continue d’écouter, le client réessaie toutes les trois secondes, quel que soit l’ordre de démarrage. La fenêtre se réduit dans la zone de notification ; le menu de l’icône permet d’ouvrir, d’arrêter ou de quitter l’application.

## Presse-papier et glisser-déposer

- Texte Unicode, HTML, RTF, PNG et DIB/DIBV5 ; extraction de texte propre lorsque le fournisseur ne propose que HTML ou RTF. **Texte sans mise en forme** retire HTML/RTF des nouvelles offres reçues.
- Fichiers, dossiers et sélections mixtes : copie seulement. Un couper-coller entre PC ne supprime jamais la source.
- Les fichiers réels sont lus en blocs de 128 Kio à la demande du destinataire. Ils ne sont pas préchargés lors du simple survol. Les descripteurs restent disponibles avant le dépôt pour les applications acceptant les fichiers virtuels, notamment les pièces jointes.
- Un glisser-déposer est annulé par Échap, retour vers la source, déconnexion, changement matériel ou reprise physique. La source reçoit Échap après capture de ses informations ; le transfert n’est autorisé qu’au dépôt sur l’autre PC.
- Les sources virtuelles, comme certains messages et pièces jointes Outlook, doivent fournir leurs données avant que leur geste d’origine soit annulé. Elles sont alors matérialisées dans le cache local de l’application : 2 Gio maximum par geste, 4 Gio au total, nettoyage des dossiers propres au programme après deux jours. Les destinations peuvent créer leur propre fichier temporaire ; l’application ne simule pas des chemins locaux inexistants.
- Limites : 4 096 éléments, noms relatifs compatibles Windows de 259 unités UTF-16, 32 Mio de formats de presse-papier, 16 catalogues conservés. Liens symboliques, jonctions, volumes entiers et chemins sortant de la sélection sont refusés. Un fichier modifié après la copie peut faire échouer sa lecture : recopier alors la sélection.

La compatibilité dépend des formats OLE exposés et acceptés par les applications. La recette Explorateur/Bureau, Codex et Outlook sur Windows est à effectuer avec [la matrice de vérification](docs/VALIDATION.md). Les fenêtres distantes, la session de connexion Windows et le bureau sécurisé UAC sont hors périmètre.

Lors d’un changement de bureau Windows (notamment UAC), le contrôle en cours est annulé. Au retour sur le bureau normal, les hooks sont réinstallés et les anciennes entrées sont libérées pour permettre un nouveau franchissement. Les fenêtres exécutées en administrateur peuvent refuser les entrées d’une application sans élévation (UIPI) : cette récupération ne leur donne pas accès.

## Réglages, confidentialité et diagnostics

Tous les fichiers de fonctionnement sont sous `%LOCALAPPDATA%\MultiMachineRust`. Ils sont distincts de ceux du produit .NET. Les identités TLS et associations sont protégées par DPAPI pour l’utilisateur Windows courant ; ne pas les copier vers l’autre PC. Le démarrage automatique utilise uniquement la valeur `MultiMachineRust` de `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.

**Diagnostics** ouvre ce dossier. Les journaux contiennent les catégories d’opérations, les files réseau et le délai mesuré, sans texte copié, noms de fichiers ni clés. Pour changer de serveur, arrêter le client puis utiliser **Nouvelle association**. Sur un serveur déjà associé, ce bouton ouvre une nouvelle association pendant deux minutes. Quitter l’application avant de supprimer ses réglages ; ne pas effacer le journal de restauration des traces avant qu’il ait restauré le réglage Windows.

Le dépôt GitHub et les releases sont **publics** depuis la décision du propriétaire du 28 octobre 2026. La documentation de référence appartient au projet Fileaify **MultiMachine-rust** (`585cb53f-078c-4783-84cb-eec5e8ac447c`). Les fichiers de ce dépôt servent de guide de construction et de preuves techniques. La publication ne change pas les droits réservés dans [LICENSE](LICENSE).

Les commits et tags utilisent une identité publique et l’adresse GitHub `noreply`, jamais l’adresse personnelle du propriétaire. Avant de contribuer ou publier, appliquer la configuration locale et les contrôles décrits dans [AGENTS.md](AGENTS.md#identité-git-et-confidentialité).

## Construction

Rust stable, cible Windows x64 et compilateur de ressources Windows. Les versions exactes des dépendances sont dans `Cargo.lock`.

Sur Windows, depuis un terminal disposant des outils Visual C++ et du Windows SDK :

```powershell
cargo test --locked
cargo build --locked --release
python scripts/package.py
```

La distribution produite ne dépend pas des outils de compilation. Aucun workflow GitHub Actions n’est nécessaire.

Sur Linux avec MinGW :

```sh
rustup target add x86_64-pc-windows-gnu
cargo test --locked
cargo clippy --all-targets -- -D warnings
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
  cargo build --locked --release --target x86_64-pc-windows-gnu
python3 scripts/package.py --target x86_64-pc-windows-gnu
```

`x86_64-w64-mingw32-{gcc,dlltool,windres}` doivent être dans le PATH. `WINDRES` permet de choisir le compilateur de ressources. Pour exécuter les tests Windows sous Wine, configurer un préfixe isolé et `CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER` ; ne pas les présenter comme des essais Windows natifs.

Sur le poste de conversion, le lanceur privé `/home/smermet/.cache/multimachine-rust-tools/cargo-run` configure la chaîne isolée. Il ne fait pas partie de l’application livrée.
