# Changelog

The extension is a thin client: what it answers comes from the `trekr` binary
it runs, whose own changelog is in the
[trekr repo](https://github.com/dpep/trekr/blob/main/CHANGELOG.md).

## Unreleased

- `trekr.features` gains `declaration`: Go to Declaration, which trekr
  0.8.9 answers (the Sorbet `.rbi` signature, else the definition). Leave it
  out to give Go to Declaration to another extension. A `trekr.features`
  you have set by hand does not list it, so add it there to keep it on.

## 0.5.1

- ERB and RABL templates are served: go to definition, hover, references
  and completion in a `.erb` (whatever language id an ERB extension gives
  it, or plain HTML) and a `.rabl`. Needs trekr 0.8.7 or newer, which says
  it reads templates. An older trekr would read a template as Ruby and mark
  its markup as syntax errors, so it is never sent one: templates are left
  alone until trekr is upgraded and restarted.

## 0.5.0

- `trekr.unresolved` decides what Go to Definition shows for a call trekr
  could not resolve: `confident` (the default) shows its guesses only when
  the first is a fair one, `peek` all of them, `best` the first, `none`
  nothing. Needs a trekr whose server reads it; an older one keeps showing
  every guess.

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
