# Validation Rust 0.1.1

État observé le 27 octobre 2026 sur le poste Linux de conversion. La documentation de référence du projet reste dans Fileaify ; ce document accompagne les tests et le binaire.

## Exécuté

- Construction Windows x64 GNU, version Rust 0.1.1, profil release, ressources icône/version/manifeste intégrées. Le manifeste demande `asInvoker`, sans élévation, et la prise en charge DPI par moniteur.
- 24 tests Linux : 19 tests du moteur et des données, 3 tests de surveillance de connexion, 2 tests réseau réels.
- 30 tests de la cible Windows exécutés sous Wine 10 dans un préfixe et un serveur X isolés : 18 tests du moteur, 3 tests de surveillance, 2 tests réseau et 7 tests Win32/COM. Le test de liens symboliques spécifique à Unix ne fait pas partie des 18 tests Windows.
- Analyse Clippy de tous les targets, dont les tests, pour Linux et Windows, avec avertissements traités comme erreurs.
- Démarrage du binaire release sous Wine ; test d’interface séparé réussi à 96, 144 et 192 DPI (100, 150 et 200 %), avec trois tailles de fenêtre à chaque DPI : contrôles dans les limites horizontales, options empilées en fenêtre étroite, accès au bas du formulaire par défilement, changement de thème et recréation de l’icône de notification. Ce contrôle n’est pas une mesure de latence sur les PC du propriétaire.
- Contrôle en lecture seule du projet .NET : les empreintes des 84 fichiers suivis, le commit de référence et la propreté de son arbre Git sont vérifiés par `scripts/verify_reference.py`.

Les résultats de la livraison doivent être renouvelés après une modification du code. Les commandes de reproduction sont dans le README et ci-dessous.

Le propriétaire a rapporté un fonctionnement fluide du contrôle et du glisser-déposer avec la 0.1.0 sur ses deux Windows. Il a ensuite signalé les icônes absentes, un formulaire tronqué, les sondes au repos, un presse-papier client → serveur défaillant et une reprise bloquée après UAC. Ces retours ne remplacent pas la recette détaillée de la 0.1.1.

## Couverture concrète

La matrice de gestes fait 50 répétitions de chaque combinaison machine physique/source, pour chaque côté d’écran, soit **400 glisser-déposer simulés par le moteur**. Elle vérifie l’absence d’un état de bouton ou de préparation résiduel. Les scénarios de récupération couvrent Échap, retour vers la source, déconnexion et changement matériel, suivis d’un nouveau geste. Une fin de nettoyage remplacée par une remise à zéro doit renouveler sa barrière ; une offre annulée reçue en retard doit rester fermée. Les demandes simultanées de contrôle doivent aboutir à un seul détenteur.

Le test réseau établit de vraies connexions TLS et UDP en boucle locale : premier appairage, refus d’un appairage supplémentaire non autorisé, rejet d’un mauvais certificat et d’un mauvais secret, activation du chemin UDP, position exacte au clic, coalescence, 32 lecteurs simultanés dans les deux directions (**64 Mio de données lus**), défilement sur le canal de contrôle pendant le transfert, refus d’un fichier non autorisé, arrêt et reconnexion en TCP puis en hybride. Les anciens objets de connexion restent volontairement vivants pendant la reconnexion pour vérifier que le port UDP est libéré malgré un ancien consommateur de fichier.

Les tests du protocole corrompent chaque octet d’un datagramme authentifié, contrôlent la fenêtre anti-rejeu, la perte et le réordonnancement de milliers de mouvements, l’ancre des actions et la dernière position fiable. Ils rejettent les tailles de trame dépassant les limites avant allocation.

Les tests COM exécutés sous Wine interrogent le vrai `IDataObject` : descripteurs Unicode, absence de faux `CF_HDROP`, énumération, capacité asynchrone, refus de lecture avant dépôt, lecture du fichier par `IStream` à travers TLS après autorisation, seek, clone indépendant, métadonnées et refus d’écriture. Ils vérifient aussi la mémoire globale, la conversion RTF multiligne par RichEdit et 50 cycles de DOWN/Reset avec libération native du bouton, puis sa libération à l’arrêt.

La régression du presse-papier répète **32 cycles réception distante → remplacement local**, en alternant Unicode et `CF_HDROP`. Elle vérifie le vrai HRESULT `S_FALSE`, l’envoi de la nouvelle offre du client vers le serveur, la lecture du fichier par TLS et la conservation d’un presse-papier local plus récent lors du nettoyage. Cette distinction `S_OK`/`S_FALSE` couvre les deux rôles, dont le code Windows est commun.

Le test réseau laisse les deux pairs au repos et vérifie que leurs compteurs TCP de contrôle et UDP restent inchangés pendant 2,1 secondes après la période de grâce. Il exige ensuite la réception du premier mouvement en moins d’une seconde et la reprise UDP. Les tests de politique couvrent l’espacement à 30 secondes, la reprise immédiate, le délai fixe de cinq secondes d’une sonde restée sans réponse et le maintien de la surveillance rapide pour une touche, un bouton ou un geste en cours.

Un test Win32 suspend l’injecteur après un DOWN effectivement envoyé : aucune nouvelle entrée ne passe pendant la suspension, le UP reste dû et est envoyé au retour, puis une nouvelle commande fonctionne. Le moteur teste l’interruption de chaque côté, avec les périphériques sur chaque PC et les deux dispositions d’écran : les deux anciens baux se ferment, les anciennes entrées sont rejetées et un nouveau bail fonctionne. **Il s’agit d’une interruption simulée, pas d’un écran sécurisé UAC réel** ; Wine ne valide pas ce comportement Windows.

Le chargement de la ressource nommée et l’affectation des petites/grandes icônes à une fenêtre sont vérifiés dans le même processus aux DPI 96, 120, 144 et 192. Wine 10 refuse le marshalling interprocessus de `WM_GETICON` ; le test d’interface saute uniquement cette lecture sous Wine, tout en la conservant sur Windows. La présence effective dans la barre des tâches et la zone de notification doit être confirmée dans le shell Windows.

## Recette à exécuter sur deux Windows physiques

Ces points **ne sont pas déclarés validés** par les essais Linux/Wine :

| Essai | Résultat attendu |
|---|---|
| Serveur A, client B ; puis périphériques sur B | Souris, cinq boutons, touches et scroll vertical/horizontal dans les deux directions |
| Mouvement continu et clics précis | Clic au dernier emplacement, pas de rattrapage prolongé ; essayer aussi TCP classique |
| HDMI présent, retiré, raccordé par KVM | Nouvelle bordure externe reconnue, profil de disposition adapté, contrôle local repris lors du changement |
| Écrans à gauche/droite, hauteur décalée, DPI 100/150/200 % | Passage seulement sur le segment commun ; ni saut dans un trou ni erreur de coordonnées |
| Copier-coller texte, HTML, RTF, PNG et image DIB | Mise en forme normale ou texte propre avec l’option correspondante |
| Copier du serveur vers le client, puis du client vers le serveur, 20 fois en alternant texte/fichiers | Chaque nouvelle copie locale remplace l’offre distante, quel que soit le côté ; aucune suppression source |
| Fichiers réels, dossiers, sélections mixtes, fichier de plusieurs centaines de Mio | Intégrité à destination, source conservée, souris réactive pendant la copie |
| Glisser-déposer : périphériques A/source A, A/source B, B/source A, B/source B | Dépôt dans Explorateur/Bureau dans chaque cas, 20 répétitions successives |
| Échap, retour source, relâchement pendant préparation, réseau coupé, KVM | Annulation sans transfert ni suppression, glisser-déposer local immédiatement réutilisable |
| Fichier vers une pièce jointe Codex et Outlook ; message/PJ Outlook en sortie | Dépôt accepté et fichier réellement disponible lorsque l’application expose/accepte les formats nécessaires |
| Fermer/réduire/restaurer, relancer Explorer, démarrer Windows dans les deux ordres | Icône de notification visible, réglages conservés, serveur à l’écoute et client en reconnexion |
| Traces du curseur, changement manuel du réglage Windows, arrêt brutal | Restauration de l’ancienne valeur sans écraser un changement manuel ; récupération au prochain lancement |
| Verrouillage, veille, déconnexion, fermeture du programme | Touches et boutons relâchés, tunnel fermé, reprise après retour de session |
| UAC sur chaque PC, pendant le contrôle puis au repos ; accepter et annuler | Ancien contrôle annulé, nouveau franchissement utilisable après retour au bureau normal ; aucune prise en charge des fenêtres administrateur |
| Fenêtre à 100/150/200 %, changement de moniteur, redimensionnement étroit | Toutes les options lisibles et accessibles, défilement vertical si nécessaire |

## Reproduire

```sh
cargo test --locked
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings
cargo test --locked --target x86_64-pc-windows-gnu --no-run
```

Sur une vraie machine Windows, `cargo test --locked -- --test-threads=1` exécute également les tests Win32. Les tests d’injection modifient l’état des boutons et celui du presse-papier remplace son contenu : utiliser une session de test sans geste en cours ni presse-papier à conserver. Sous Wine, utiliser un préfixe et un affichage isolés avec un runner explicitement configuré.

Le test d’interface s’exécute séparément après une construction release : définir `MMR_UI_EXE` avec le chemin Windows absolu du binaire, puis `cargo test --locked --test windows_ui -- --ignored --test-threads=1 --nocapture`. `MMR_EXPECT_DPI` permet d’exiger le DPI réel de la fenêtre. Il lance et ferme sa propre instance ; ne pas laisser une autre instance ouverte dans cette session.

La recette de deux PC et les performances corporate/KVM restent à relever sur le matériel réel. Cette livraison est donc une **préversion**, et la version .NET validée reste disponible indépendamment.
