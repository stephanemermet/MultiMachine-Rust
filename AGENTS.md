# MultiMachine Rust

Projet indépendant, autorisé par le propriétaire le 27 octobre 2026. Ne modifier aucun fichier, réglage, livrable, branche ou dépôt du projet .NET voisin. Sa 0.3.7 sert uniquement de référence fonctionnelle. Aucun besoin de compatibilité réseau ou de migration automatique entre les deux applications.

- Application Windows 10/11 x64 portable, sans élévation ; réseau LAN et copie seulement, jamais de suppression source.
- Protocole, association, préférences, cache, démarrage et mutex propres à MultiMachine Rust.
- Dépôt GitHub et livrables publics sur décision du propriétaire du 28 octobre 2026, distincts du dépôt .NET. Aucun secret ni donnée personnelle privée dans les sources, URL, journaux ou publications ; utiliser uniquement les accès explicitement autorisés.
- Reprendre les régressions du contrôle, des gestes, des pertes réseau et des nettoyages dans les tests du moteur. Tests et compilation Linux ne prouvent pas une recette sur deux Windows.
- Ne pas présenter une fonctionnalité non reliée à l’application comme implémentée. Documenter les limites et les essais exécutés.

La documentation de référence est dans le projet Fileaify **MultiMachine-rust**, identifiant `585cb53f-078c-4783-84cb-eec5e8ac447c`. Lire https://fileaify.com/agent, le contrat courant, les documents `always` et les documents pertinents avant une intervention. Le dépôt conserve le guide de construction et les preuves techniques. Les contributions en attente ne sont pas publiées. Ne pas modifier le projet documentaire .NET.

## Identité Git et confidentialité

Ne jamais publier l’adresse e-mail réelle du propriétaire ni une autre donnée personnelle privée dans les champs auteur, committer, tagger, les messages, les trailers de commit, les fichiers ou les releases. Le pseudonyme GitHub public est autorisé. Le propriétaire a aussi accepté les chemins de compilation contenant `/home/smermet` ; ne pas élargir cette exception à d’autres données.

Avant de créer un commit ou un tag, configurer explicitement chaque clone, y compris celui de l’exécuteur :

```sh
git config --local user.name stephanemermet
git config --local user.email 51788154+stephanemermet@users.noreply.github.com
git config --local user.useConfigOnly true
git config --local core.hooksPath .githooks
```

Ne pas hériter de l’adresse globale du poste. Avant tout push, exécuter `python3 scripts/check_git_identity.py` et examiner les fichiers/livrables pour exclure les données privées ; le script contrôle uniquement les identités et adresses e-mail dans les objets commit/tag, pas le contenu des fichiers. Le hook `pre-push` vérifie aussi tout l’historique des références réellement envoyées. Python 3 est requis sur le poste de publication. Ne pas contourner un refus et ne pas réintroduire un ancien historique. Une autre identité publique de contributeur doit être ajoutée explicitement à la liste autorisée après vérification.
