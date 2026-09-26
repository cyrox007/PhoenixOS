# Document and Media Platform

PhoenixOS should not implement each document format, image codec, container, and video codec independently inside every application. The desktop needs a shared platform layer that applications can reuse.

## Architecture

```text
Writer / Sheets / Slides / Documents
Images / Paint / Media / Browser
              |
              v
+----------------------------------+
| Phoenix document/media services  |
+----------------------------------+
| MIME/file associations           |
| text layout / fonts              |
| document import/export           |
| PDF export/view bridge           |
| image codec abstraction          |
| media demux/decode abstraction   |
| subtitles                        |
| color-management hooks           |
| thumbnail/preview service        |
| print/export service             |
+----------------------------------+
              |
              v
ported mature libraries + native OS APIs
```

## Document model

PhoenixOS should prefer open, documented formats for its first-party authoring path.

Primary authoring targets:

- ODT for formatted text documents;
- ODS for spreadsheets;
- ODP for presentations.

Interoperability targets:

- DOCX;
- XLSX;
- PPTX;
- RTF;
- CSV;
- PDF.

PDF is primarily a viewing/export format. Editing arbitrary PDFs is not a 1.0 requirement.

The office applications should use an internal document model independent from any single external format so import/export code can evolve without rewriting the UI.

## Text layout

The common text stack must eventually provide:

- Unicode;
- bidirectional text;
- shaping;
- font fallback;
- line breaking;
- rich text;
- styled spans;
- paragraph layout;
- pagination;
- printing/PDF output.

This same stack benefits the desktop shell, browser, document editors, terminal, and accessibility support.

## Image stack

Initial decode/encode targets:

- PNG;
- JPEG;
- WebP;
- BMP;
- GIF;
- SVG.

Later formats can include AVIF and additional camera/professional formats through packages.

The image layer should expose decoded surfaces in a format suitable for CPU rendering initially and GPU upload later.

## Media stack

Initial containers:

- MP4;
- Matroska/MKV;
- WebM.

Codec support should come from mature third-party codec libraries. PhoenixOS does not implement modern patented/complex codecs from scratch.

Target decode support, subject to the chosen distribution/build policy:

- H.264;
- H.265/HEVC;
- VP9;
- AV1;
- AAC;
- Opus;
- Vorbis;
- common PCM formats.

The architecture separates:

1. demuxing;
2. decoding;
3. audio/video synchronization;
4. subtitle decoding/rendering;
5. output surfaces/audio streams.

This makes the same stack usable by Phoenix Media and Phoenix Browser.

## Hardware acceleration

Software decoding is the first correctness target.

Later, graphics/media drivers may expose hardware decode acceleration. Applications should not depend directly on GPU-vendor APIs; the shared media layer chooses hardware or software decoding.

## Parser and codec isolation

Documents, images, fonts, subtitles, containers, and codecs process untrusted input.

Where practical, complex parsers/decoders should execute in sandboxed worker processes with:

- restricted filesystem access;
- limited device access;
- bounded memory;
- IPC-based input/output;
- crash containment.

A malformed media file should not be able to crash the desktop session or kernel.

## Application integration

The File Manager should use the shared stack for:

- thumbnails;
- metadata;
- previews;
- default application selection.

The Browser should reuse:

- image decoding where practical;
- audio/video output;
- codecs;
- file picker;
- PDF viewing integration.

The office applications should reuse:

- fonts/text shaping;
- image import;
- print/PDF export;
- clipboard and drag/drop;
- common file dialogs.

## Porting strategy

The project should prefer porting mature libraries for complex format support instead of recreating decades of file-format and codec behavior.

The POSIX/libc compatibility layer is therefore also a productivity/media enabler: it reduces the effort required to bring document, image, PDF, multimedia, font, and browser dependencies to PhoenixOS.
