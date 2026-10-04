import { createRequire } from "node:module";
import { describe, expect, test } from "vitest";

/*
 * The package's Babel plugin (babel-plugin.cjs) on hand-made AST nodes, with the node builders of `@babel/types` as
 * plain objects: the package does not depend on Babel (the app's Babel runs the plugin; the playground's Metro build
 * proves it there).
 */

type Node = { readonly type: string } & Record<string, unknown>;

const types = {
  identifier: (name: string): Node => ({ type: "Identifier", name }),
  stringLiteral: (value: string): Node => ({ type: "StringLiteral", value }),
  objectProperty: (key: Node, value: Node): Node => ({ type: "ObjectProperty", key, value }),
  objectExpression: (properties: Node[]): Node => ({ type: "ObjectExpression", properties }),
  importDeclaration: (specifiers: Node[], source: Node): Node => ({ type: "ImportDeclaration", specifiers, source }),
  importNamespaceSpecifier: (local: Node): Node => ({ type: "ImportNamespaceSpecifier", local }),
  exportNamedDeclaration: (declaration: Node | null, specifiers: Node[], source: Node | null = null): Node => ({
    type: "ExportNamedDeclaration",
    declaration,
    specifiers,
    source,
  }),
  exportSpecifier: (local: Node, exported: Node): Node => ({ type: "ExportSpecifier", local, exported }),
};

interface Visitor {
  MetaProperty(path: Path): void;
  ExportNamedDeclaration(path: Path): void;
}

interface Path {
  node: Node;
  replaced: Node | null;
  replacedWith: Node[] | null;
  scope: { generateUidIdentifier(name: string): Node };
  replaceWith(node: Node): void;
  replaceWithMultiple(nodes: Node[]): void;
}

const plugin = createRequire(import.meta.url)("../babel-plugin.cjs") as (babel: { types: typeof types }) => { visitor: Visitor };
const { visitor } = plugin({ types });

function pathOf(node: Node): Path {
  const path: Path = {
    node,
    replaced: null,
    replacedWith: null,
    scope: { generateUidIdentifier: (name) => types.identifier(`_${name}`) },
    replaceWith(next) {
      path.replaced = next;
    },
    replaceWithMultiple(next) {
      path.replacedWith = next;
    },
  };
  return path;
}

describe("babel-plugin", () => {
  test("import.meta becomes { url: undefined }", () => {
    const path = pathOf({ type: "MetaProperty", meta: types.identifier("import"), property: types.identifier("meta") });
    visitor.MetaProperty(path);
    expect(path.replaced).toEqual(types.objectExpression([types.objectProperty(types.identifier("url"), types.identifier("undefined"))]));
    const other = pathOf({ type: "MetaProperty", meta: types.identifier("new"), property: types.identifier("target") });
    visitor.MetaProperty(other);
    expect(other.replaced).toBeNull();
  });

  test('export * as ns from "m" becomes import * as _ns from "m"; export { _ns as ns }, the rest kept', () => {
    const source = types.stringLiteral("./codecs.js");
    const namespace: Node = { type: "ExportNamespaceSpecifier", exported: types.identifier("codecs") };
    const path = pathOf(types.exportNamedDeclaration(null, [namespace], source));
    visitor.ExportNamedDeclaration(path);
    expect(path.replacedWith).toEqual([
      types.importDeclaration([types.importNamespaceSpecifier(types.identifier("_codecs"))], types.stringLiteral("./codecs.js")),
      types.exportNamedDeclaration(null, [types.exportSpecifier(types.identifier("_codecs"), types.identifier("codecs"))]),
    ]);
    // A plain re-export, and a local export, are left alone.
    const plain = pathOf(types.exportNamedDeclaration(null, [types.exportSpecifier(types.identifier("a"), types.identifier("a"))], source));
    visitor.ExportNamedDeclaration(plain);
    expect(plain.replacedWith).toBeNull();
    const local = pathOf(types.exportNamedDeclaration(null, [types.exportSpecifier(types.identifier("a"), types.identifier("a"))]));
    visitor.ExportNamedDeclaration(local);
    expect(local.replacedWith).toBeNull();
  });
});
