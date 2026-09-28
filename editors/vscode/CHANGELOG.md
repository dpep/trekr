# Changelog

The extension is a thin client: what it answers comes from the `trekr` binary
it runs, whose own changelog is in the
[trekr repo](https://github.com/dpep/trekr/blob/main/CHANGELOG.md).

## 0.4.0

- First Marketplace release, with an icon.
- Recommends trekr 0.5.0 or newer, whose server understands RSpec: `describe`,
  `it` and `let` blocks, `let`/`subject` names and `expect(...).to`.

## 0.3.0

- Go to Definition and Find References on local, instance and class
  variables; `documentHighlight`, toggled by `trekr.features`.

## 0.2.0

- `trekr.referenceLimit` caps Find All References; `require` strings open the
  file they load and are underlined as links.

## 0.1.0

- Definition, references, hover, completion, outline, workspace symbols,
  implementation, call hierarchy and syntax errors from the trekr language
  server, with background indexing (`trekr.index`) and `trekr.features`.
