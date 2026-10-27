# Validation de la première préversion Rust

État observé le 27 octobre 2026 sur le poste Linux de conversion. La documentation de référence du projet reste dans Fileaify ; ce document accompagne les tests et le binaire.

## Exécuté

- Construction Windows x64 GNU, version Rust 0.1.0, profil release, ressources icône/version/manifeste intégrées. Le manifeste demande `asInvoker`, sans élévation, et la prise en charge DPI par moniteur.
- 20 tests Linux : 18 tests du moteur et des données, 2 tests réseau réels.
- 23 tests de la cible Windows exécutés sous Wine 10 dans un préfixe et un serveur X isolés : 17 tests du moteur, 2 tests réseau et 4 tests Win32/COM. Le test de liens symboliques spécifique à Unix ne fait pas partie des 17 tests Windows.
- Analyse Clippy de tous les targets, dont les tests, pour Linux et Windows, avec avertissements traités comme erreurs.
- Démarrage de l’interface Win32 sous Wine ; inspection visuelle des commandes, réglages et affichage. Ce contrôle n’est pas une mesure de latence sur les PC du propriétaire.
- Contrôle en lecture seule du projet .NET : les empreintes des 84 fichiers suivis, le commit de référence et la propreté de son arbre Git sont vérifiés par `scripts/verify_reference.py`.

Les résultats de la livraison doivent être renouvelés après une modification du code. Les commandes de reproduction sont dans le README et ci-dessous.

## Couverture concrète

La matrice de gestes fait 50 répétitions de chaque combinaison machine physique/source, pour chaque côté d’écran, soit **400 glisser-déposer simulés par le moteur**. Elle vérifie l’absence d’un état de bouton ou de préparation résiduel. Les scénarios de récupération couvrent Échap, retour vers la source, déconnexion et changement matériel, suivis d’un nouveau geste. Une fin de nettoyage remplacée par une remise à zéro doit renouveler sa barrière ; une offre annulée reçue en retard doit rester fermée. Les demandes simultanées de contrôle doivent aboutir à un seul détenteur.

Le test réseau établit de vraies connexions TLS et UDP en boucle locale : premier appairage, refus d’un appairage supplémentaire non autorisé, rejet d’un mauvais certificat et d’un mauvais secret, activation du chemin UDP, position exacte au clic, coalescence, 32 lecteurs simultanés dans les deux directions (**64 Mio de données lus**), défilement sur le canal de contrôle pendant le transfert, refus d’un fichier non autorisé, arrêt et reconnexion en TCP puis en hybride. Les anciens objets de connexion restent volontairement vivants pendant la reconnexion pour vérifier que le port UDP est libéré malgré un ancien consommateur de fichier.

Les tests du protocole corrompent chaque octet d’un datagramme authentifié, contrôlent la fenêtre anti-rejeu, la perte et le réordonnancement de milliers de mouvements, l’ancre des actions et la dernière position fiable. Ils rejettent les tailles de trame dépassant les limites avant allocation.

Les tests COM exécutés sous Wine interrogent le vrai `IDataObject` : descripteurs Unicode, absence de faux `CF_HDROP`, énumération, capacité asynchrone, refus de lecture avant dépôt, lecture du fichier par `IStream` à travers TLS après autorisation, seek, clone indépendant, métadonnées et refus d’écriture. Ils vérifient aussi la mémoire globale, la conversion RTF multiligne par RichEdit et 50 cycles de DOWN/Reset avec libération native du bouton, puis sa libération à l’arrêt.

## Recette à exécuter sur deux Windows physiques

Ces points **ne sont pas déclarés validés** par les essais Linux/Wine :

| Essai | Résultat attendu |
|---|---|
| Serveur A, client B ; puis périphériques sur B | Souris, cinq boutons, touches et scroll vertical/horizontal dans les deux directions |
| Mouvement continu et clics précis | Clic au dernier emplacement, pas de rattrapage prolongé ; essayer aussi TCP classique |
| HDMI présent, retiré, raccordé par KVM | Nouvelle bordure externe reconnue, profil de disposition adapté, contrôle local repris lors du changement |
| Écrans à gauche/droite, hauteur décalée, DPI 100/150/200 % | Passage seulement sur le segment commun ; ni saut dans un trou ni erreur de coordonnées |
| Copier-coller texte, HTML, RTF, PNG et image DIB | Mise en forme normale ou texte propre avec l’option correspondante |
| Fichiers réels, dossiers, sélections mixtes, fichier de plusieurs centaines de Mio | Intégrité à destination, source conservée, souris réactive pendant la copie |
| Glisser-déposer : périphériques A/source A, A/source B, B/source A, B/source B | Dépôt dans Explorateur/Bureau dans chaque cas, 20 répétitions successives |
| Échap, retour source, relâchement pendant préparation, réseau coupé, KVM | Annulation sans transfert ni suppression, glisser-déposer local immédiatement réutilisable |
| Fichier vers une pièce jointe Codex et Outlook ; message/PJ Outlook en sortie | Dépôt accepté et fichier réellement disponible lorsque l’application expose/accepte les formats nécessaires |
| Fermer/réduire/restaurer, relancer Explorer, démarrer Windows dans les deux ordres | Icône de notification visible, réglages conservés, serveur à l’écoute et client en reconnexion |
| Traces du curseur, changement manuel du réglage Windows, arrêt brutal | Restauration de l’ancienne valeur sans écraser un changement manuel ; récupération au prochain lancement |
| Verrouillage, veille, déconnexion, fermeture du programme | Touches et boutons relâchés, tunnel fermé, reprise après retour de session |

## Reproduire

```sh
cargo test --locked
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings
cargo test --locked --target x86_64-pc-windows-gnu --no-run
```

Sur une vraie machine Windows, `cargo test --locked -- --test-threads=1` exécute également les tests Win32. Le test d’injection déplace l’état du bouton gauche ; le lancer dans une session de test sans geste en cours. Sous Wine, utiliser un préfixe et un affichage isolés avec un runner explicitement configuré.

La recette de deux PC et les performances corporate/KVM restent à relever sur le matériel réel. Cette livraison est donc une **préversion**, et la version .NET validée reste disponible indépendamment.
