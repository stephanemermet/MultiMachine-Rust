# Protocole et invariants testables

Ce document décrit le protocole Rust lié à ce code. La documentation de référence et les décisions de projet sont dans Fileaify, projet `585cb53f-078c-4783-84cb-eec5e8ac447c`.

## Connexion

Identifiant `multimachine-rust/1`, indépendant du produit .NET. Deux connexions TLS au même port : `control` puis `bulk`, liées par UUID, secret d’association, IP et délai de 12 secondes. Le certificat autosigné est épinglé après une comparaison de code et confirmation humaine. Les secrets d’association et privés ne sont jamais journalisés. Écoute IPv4 LAN, connexions client sortantes, découverte par broadcast avec nonce aléatoire.

Chaque trame TCP est un JSON UTF-8 précédé de sa longueur non signée de 32 bits en ordre réseau. Limites : contrôle 256 Kio, transfert 48 Mio. Une erreur de validation ferme la session. Les messages `Read` autorisent uniquement une offre connue, un identifiant de fichier connu, une position et un bloc borné ; ils ne transportent jamais de chemin à ouvrir.

Le catalogue tient son verrou seulement pendant l’accès à son index. Le parcours de répertoires et la lecture du disque se font sans bloquer les hooks. Deux canaux TLS empêchent un gros presse-papier ou un transfert de placer la souris derrière ses octets.

## Mouvement hybride

Le canal fiable numérote ses actions. Chaque mouvement porte un numéro croissant et la dernière action qu’il suit. Le récepteur ne conserve que le mouvement futur le plus récent, jusqu’à réception de cette action. Chaque action fiable apporte aussi sa dernière position antérieure. Une position déjà dépassée ne déplace pas à nouveau le curseur. Une position finale est envoyée une seule fois après 60 ms sans mouvement ; il ne s’agit pas d’une photographie périodique de toutes les touches.

Le datagramme fait 85 octets : magic `MRU1`, UUID de session sur 16 octets, compteur réseau sur 8 octets, puis message chiffré AES-256-GCM et tag de 16 octets. L’en-tête est authentifié. Clés distinctes par direction, nouvelles à chaque session. Nonce de 12 octets constitué de quatre zéros et du compteur ; le dépassement ferme le chemin. Fenêtre anti-rejeu de 64 numéros mise à jour après authentification seulement. Les UUID utilisent leur représentation d’octets Rust standard, sans compatibilité avec les GUID .NET.

Une sonde client initialise le chemin UDP sans ouverture entrante côté client. Sonde toutes les 100 ms quand le chemin répond ; réponse périmée après 350 ms ; nouvelle tentative chaque seconde si indisponible. La coalescence remplace uniquement des mouvements adjacents du même bail. Les clics, touches, défilements et transitions ne sont pas supprimés. Le récepteur écarte les événements d’un ancien bail ou d’une ancienne connexion.

## Contrôle et gestes

Le rôle réseau ne change pas lorsque les périphériques sont déplacés par un KVM. Un bail identifie le contrôle actif ; le serveur arbitre les demandes simultanées. Les hooks ignorent les entrées injectées par le programme. Les défilements synthétisés par les pilotes de trackpad restent admissibles, contrairement aux propres événements tagués du programme.

Les événements réseau passent directement au thread de contrôle, sans attendre la fenêtre principale. L’injection est sérialisée sur un autre thread ; une remise à zéro libère les boutons réellement injectés, y compris un DOWN encore en cours lors de l’annulation. Les callbacks de barrières portent une génération : une ancienne fin ne peut pas libérer un bouton d’un geste suivant.

Le glisser-déposer suit capture → offre → préparation → staging → natif → dépôt ou annulation → nettoyage. Un retour depuis la machine contrôlée vers celle qui possède les périphériques utilise une transition dédiée. Le bouton DOWN de préparation est injecté sur une fenêtre de passage propre au programme, puis le curseur est placé huit pixels à l’intérieur avant `DoDragDrop`. L’acquittement de préparation précède la boucle OLE, afin de libérer la retransmission des mouvements.

Les fenêtres de passage n’acceptent aucun dépôt. Elles capturent le vrai `IDataObject`, sécurisent les éventuels contenus virtuels, demandent Échap puis attendent `DragLeave`. À destination, `IDataObject` expose les descripteurs et `IStream`, jamais un faux `CF_HDROP`. Les lectures attendent la décision de dépôt. Un watchdog indépendant de la boucle OLE cible uniquement la capture appartenant au thread du programme ; il ne prend pas la capture d’Explorer et n’envoie pas un Échap global tardif. Les offres fermées sont mémorisées pour rejeter une annonce tardive sur le canal de fichiers.

## Séparation Windows

Threads dédiés : interface, hooks/machine à états, injection, presse-papier STA, glisser-déposer STA, watchdog, exécuteur réseau Tokio. Aucun thread de hook ne doit faire de lecture de fichier, d’appel COM au fournisseur ou d’attente réseau. Les indicateurs de session et identifiants de connexion rejettent les événements d’une session remplacée.
