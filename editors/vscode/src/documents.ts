// The documents the server answers: Ruby, and the view templates whose Ruby
// it reads — ERB under whichever language id an ERB extension gives it, or
// plain HTML for a `.erb` with none, and RABL, which no common extension
// names.
export const DOCUMENTS = [
  { scheme: "file", language: "ruby" },
  { scheme: "file", language: "erb" },
  { scheme: "file", language: "html.erb" },
  { scheme: "file", pattern: "**/*.erb" },
  { scheme: "file", pattern: "**/*.rabl" },
];
