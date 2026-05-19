# Changelog

Toutes les modifications notables de Wisplet sont consignées ici.

Le format suit [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/),
et le projet adhère au [versionnage sémantique](https://semver.org/lang/fr/).

## [0.2.0] — 2026-05-19

### Ajouté

- Gestion des raccourcis depuis l'interface : ajout, édition et suppression
  d'apps sans éditer le fichier JSON à la main.
- Menu contextuel au clic droit, adapté à la zone : sur une tuile
  (Modifier / Supprimer / Ajouter), sur un cadre (Ajouter dans ce groupe),
  dans le vide (Ajouter).
- Sélecteur de fichier natif pour choisir l'exécutable d'une app.
- Auto-remplissage à la sélection d'un `.exe` / `.lnk` : extraction de l'icône
  (Win32) et du titre depuis le champ `FileDescription` de la version-info.
- Fichier d'apps propriétaire `~/.config/wisplet/apps.json`, initialisé une
  seule fois depuis la configuration YASB existante.

### Modifié

- Le menu contextuel natif de WebView2 et les raccourcis de page
  (rechargement, impression, navigation arrière) sont désactivés.

## [0.1.0]

- Version initiale : launcher overlay, toggle global, grille d'apps par
  sections, paramètres persistés, raccourci global rebindable.
