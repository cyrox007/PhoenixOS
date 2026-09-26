# Bundled Applications

The initial desktop should feel usable rather than like a kernel demo. PhoenixOS 1.0 therefore includes both system utilities and a practical minimum of productivity/media applications.

## Core system applications

- File Manager
- Terminal
- plain-text/code editor
- Settings
- System Monitor / Task Manager
- Logs / diagnostics viewer
- Calculator
- archive utility
- package/software manager
- updater UI
- disk/storage utility
- network settings
- Phoenix Browser

## Documents and office-style work

### Phoenix Writer

A formatted document editor for ordinary letters, notes, reports, and longer documents.

Target capabilities:

- rich text styles;
- paragraphs/headings/lists;
- tables;
- images embedded in documents;
- headers/footers;
- page layout and print/export path;
- spell-check integration point;
- PDF export.

Primary open format target: ODT.

Compatibility targets: DOCX and RTF import/export as practical porting support becomes available.

### Phoenix Sheets

A spreadsheet viewer/editor with:

- cells and ranges;
- formulas;
- formatting;
- multiple sheets;
- CSV import/export;
- basic charts.

Primary open format target: ODS.

Compatibility target: XLSX.

### Phoenix Slides

A presentation viewer/editor with:

- slides;
- text and images;
- simple shapes;
- themes/layouts;
- presentation mode;
- PDF export.

Primary open format target: ODP.

Compatibility target: PPTX.

### Phoenix Documents

A document/PDF viewer handling PDF plus supported read-only office/document formats where appropriate.

The viewer/editor applications should share common text layout, printing/export, MIME/file-association, recent-files, thumbnail, and document-conversion services rather than each reimplementing them.

## Images and graphics

### Phoenix Images

Image viewer with:

- PNG;
- JPEG;
- WebP;
- BMP;
- GIF;
- SVG;
- EXIF/orientation handling;
- zoom/pan;
- slideshow;
- thumbnails.

### Phoenix Paint

A lightweight image editor. It is not intended to replace professional graphics suites in 1.0.

Minimum editing target:

- crop;
- resize;
- rotate/flip;
- basic drawing/annotation;
- text;
- simple selections;
- brightness/contrast/basic color adjustment;
- save/export to common image formats.

More advanced raster/vector editors can later be ported as third-party applications.

## Audio and video

### Phoenix Media

A general media player for local files and network streams.

Minimum UX:

- play/pause/seek;
- playlists;
- subtitles;
- audio-track selection;
- fullscreen;
- volume/mute;
- playback speed;
- hardware-decoding path when graphics/video drivers eventually expose it.

Initial container targets include MP4, MKV, and WebM. Codec support should come from mature ported multimedia libraries rather than PhoenixOS writing its own H.264/H.265/VP9/AV1/AAC/Opus implementations.

The media stack should be reusable by Phoenix Browser and other applications.

## Shared document/media platform

PhoenixOS should expose reusable services/libraries for:

- MIME type detection and file associations;
- thumbnails/previews;
- fonts and text shaping;
- color management hooks;
- image decode/encode;
- media demux/decode;
- audio/video clocks and synchronization;
- subtitle parsing/rendering;
- printing/PDF export;
- clipboard/drag-and-drop;
- recent files;
- file pickers;
- sandboxed codec/parser execution where useful.

Large parsers and codecs are treated as attack surfaces and should be isolated where the process model permits.

## Developer-facing tools

- shell;
- core file/process utilities;
- compiler/SDK integration on a development install;
- debugger;
- profiler/tracing viewer;
- package builder.

## Games

At least two games are a 1.0 requirement.

A Snake/Tetris-class 2D game will exercise event loop, keyboard input, timers, 2D rendering, window lifecycle, and basic audio.

A small platformer or top-down game will exercise continuous rendering, asset loading, simultaneous inputs, audio mixing, frame pacing, and more demanding 2D graphics.

Later, a lightweight Game SDK can wrap surfaces/input/audio and provide a clean example for third-party developers.
