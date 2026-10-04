// A Babel plugin for React Native apps that use @undra/runtime (docs/REACT_NATIVE.md, "Install").
//
// Hermes cannot compile `import.meta`, and @undra/runtime's `wasm-worker` mode reaches for
// `import.meta.url` to find its worker script. That mode never runs under React Native (there is no
// WebAssembly and no Worker), but Metro bundles every module the runtime's entry point reaches, so
// the expression must still compile. This plugin replaces `import.meta` with `{ url: undefined }`.
//
// It also rewrites `export * as ns from "m"` (ES2020, which @undra/runtime uses for `codecs`) as the ES2015
// pair `import * as _ns from "m"; export { _ns as ns };`: the React Native preset's module transform does
// not take the first form, and Metro stops with "Export namespace should be first transformed by
// @babel/plugin-transform-export-namespace-from". Nothing else changes.
//
//   // babel.config.js
//   module.exports = {
//     presets: ['module:@react-native/babel-preset'],
//     plugins: ['@undra/react-native/babel-plugin'],
//   };
"use strict";

module.exports = function undraReactNative({ types: t }) {
  return {
    name: "@undra/react-native",
    visitor: {
      MetaProperty(path) {
        const { meta, property } = path.node;
        if (meta.name === "import" && property.name === "meta") {
          path.replaceWith(t.objectExpression([t.objectProperty(t.identifier("url"), t.identifier("undefined"))]));
        }
      },
      ExportNamedDeclaration(path) {
        const { node } = path;
        const namespace = node.specifiers.find((s) => s.type === "ExportNamespaceSpecifier");
        if (namespace === undefined || !node.source) return;
        const exported = namespace.exported;
        const local = path.scope.generateUidIdentifier(exported.type === "Identifier" ? exported.name : "namespace");
        const replacement = [
          t.importDeclaration([t.importNamespaceSpecifier(local)], t.stringLiteral(node.source.value)),
          t.exportNamedDeclaration(null, [t.exportSpecifier(t.identifier(local.name), exported)]),
        ];
        const rest = node.specifiers.filter((s) => s !== namespace);
        if (rest.length > 0) replacement.push(t.exportNamedDeclaration(null, rest, t.stringLiteral(node.source.value)));
        path.replaceWithMultiple(replacement);
      },
    },
  };
};
