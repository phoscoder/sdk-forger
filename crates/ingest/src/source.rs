//! Source analysis with tree-sitter: the public surface of an existing SDK (Port mode).
use tree_sitter::{Language, Node, Parser};

use crate::IngestError;

/// Languages Port can read (source) and write (target).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Python,
    Node,
    Php,
    Rust,
    Go,
}

impl Lang {
    pub fn parse(s: &str) -> Option<Lang> {
        match s.to_lowercase().as_str() {
            "python" | "py" => Some(Lang::Python),
            "node" | "nodejs" | "javascript" | "js" | "typescript" | "ts" => Some(Lang::Node),
            "php" => Some(Lang::Php),
            "rust" | "rs" => Some(Lang::Rust),
            "go" | "golang" => Some(Lang::Go),
            _ => None,
        }
    }

    /// Canonical short name, as used on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Lang::Python => "python",
            Lang::Node => "node",
            Lang::Php => "php",
            Lang::Rust => "rust",
            Lang::Go => "go",
        }
    }

    pub fn display(self) -> &'static str {
        match self {
            Lang::Python => "Python",
            Lang::Node => "Node.js",
            Lang::Php => "PHP",
            Lang::Rust => "Rust",
            Lang::Go => "Go",
        }
    }

    fn extensions(self) -> &'static [&'static str] {
        match self {
            Lang::Python => &["py"],
            Lang::Node => &["js", "mjs", "cjs", "ts"],
            Lang::Php => &["php"],
            Lang::Rust => &["rs"],
            Lang::Go => &["go"],
        }
    }

    pub fn is_test_file(self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        let in_dir = |dirs: &[&str]| path.split('/').rev().skip(1).any(|d| dirs.contains(&d));
        match self {
            Lang::Python => {
                name.starts_with("test_")
                    || name.ends_with("_test.py")
                    || in_dir(&["tests", "test"])
            }
            Lang::Node => {
                name.contains(".test.")
                    || name.contains(".spec.")
                    || in_dir(&["test", "tests", "__tests__", "spec"])
            }
            Lang::Php => name.ends_with("Test.php") || in_dir(&["tests", "test"]),
            Lang::Rust => in_dir(&["tests"]),
            Lang::Go => name.ends_with("_test.go"),
        }
    }

    /// Any code file of this language (tests included), outside vendored/build/example directories.
    pub fn is_code_file(self, path: &str) -> bool {
        path.rsplit_once('.')
            .is_some_and(|(_, e)| self.extensions().contains(&e))
            && !ignored(path)
    }

    /// A non-test source file of this language, outside vendored/build/example directories.
    pub fn is_source_file(self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        let ext_ok = path
            .rsplit_once('.')
            .is_some_and(|(_, e)| self.extensions().contains(&e));
        let config =
            matches!(name, "setup.py" | "conftest.py" | "build.rs") || name.contains(".config.");
        ext_ok && !config && !ignored(path) && !self.is_test_file(path)
    }
}

fn ignored(path: &str) -> bool {
    const DIRS: [&str; 10] = [
        "node_modules",
        "vendor",
        "dist",
        "build",
        "target",
        "__pycache__",
        "examples",
        "example",
        "docs",
        "benches",
    ];
    path.split('/')
        .any(|s| DIRS.contains(&s) || s.starts_with('.'))
        || path.ends_with(".d.ts")
        || path.ends_with(".min.js")
}

/// Guess the language of a repo: manifest files first, then the most common source extension.
pub fn detect_lang(files: &[String]) -> Option<Lang> {
    let root = |f: &str| files.iter().any(|p| p == f);
    if root("Cargo.toml") {
        return Some(Lang::Rust);
    }
    if root("go.mod") {
        return Some(Lang::Go);
    }
    if root("composer.json") {
        return Some(Lang::Php);
    }
    if root("package.json") {
        return Some(Lang::Node);
    }
    if root("pyproject.toml") || root("setup.py") || root("requirements.txt") {
        return Some(Lang::Python);
    }
    [Lang::Python, Lang::Node, Lang::Php, Lang::Rust, Lang::Go]
        .into_iter()
        .map(|l| (files.iter().filter(|f| l.is_source_file(f)).count(), l))
        .filter(|(n, _)| *n > 0)
        .max_by_key(|(n, _)| *n)
        .map(|(_, l)| l)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Class,
    Function,
    Method,
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub kind: Kind,
    pub name: String,
    pub signature: String,
    pub doc: Option<String>,
    pub parent: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FileOutline {
    pub path: String,
    pub symbols: Vec<Symbol>,
}

pub fn outline_python(path: &str, src: &str) -> Result<FileOutline, IngestError> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .map_err(|e| IngestError::Invalid(e.to_string()))?;
    let tree = parser
        .parse(src, None)
        .ok_or_else(|| IngestError::Invalid(format!("{path}: parse failed")))?;
    let mut symbols = Vec::new();
    collect(tree.root_node(), src, None, &mut symbols);
    Ok(FileOutline {
        path: path.to_string(),
        symbols,
    })
}

fn text<'a>(n: Node, src: &'a str) -> &'a str {
    &src[n.byte_range()]
}

fn collect<'a>(node: Node, src: &'a str, parent: Option<&'a str>, out: &mut Vec<Symbol>) {
    let mut cur = node.walk();
    for child in node.named_children(&mut cur) {
        let def = if child.kind() == "decorated_definition" {
            child.child_by_field_name("definition")
        } else {
            Some(child)
        };
        let Some(def) = def else { continue };
        let Some(name_node) = def.child_by_field_name("name") else {
            continue;
        };
        let name = text(name_node, src);
        match def.kind() {
            // Nested classes are internal detail; only top-level ones are public API.
            "class_definition" if parent.is_none() && !name.starts_with('_') => {
                let bases = def
                    .child_by_field_name("superclasses")
                    .map_or("", |b| text(b, src));
                out.push(Symbol {
                    kind: Kind::Class,
                    name: name.to_string(),
                    signature: format!("class {name}{bases}"),
                    doc: doc(def, src),
                    parent: None,
                });
                if let Some(body) = def.child_by_field_name("body") {
                    collect(body, src, Some(name), out);
                }
            }
            "function_definition" if !name.starts_with('_') || name == "__init__" => {
                let params = def
                    .child_by_field_name("parameters")
                    .map_or("()", |p| text(p, src));
                let ret = def
                    .child_by_field_name("return_type")
                    .map_or(String::new(), |r| format!(" -> {}", text(r, src)));
                let asyn = if text(def, src).starts_with("async") {
                    "async "
                } else {
                    ""
                };
                out.push(Symbol {
                    kind: if parent.is_some() {
                        Kind::Method
                    } else {
                        Kind::Function
                    },
                    name: name.to_string(),
                    signature: format!("{asyn}def {name}{params}{ret}"),
                    doc: doc(def, src),
                    parent: parent.map(str::to_string),
                });
            }
            _ => {}
        }
    }
}

/// First line of the docstring, if the body starts with one.
fn doc(def: Node, src: &str) -> Option<String> {
    let body = def.child_by_field_name("body")?;
    let first = body.named_child(0)?;
    if first.kind() != "expression_statement" {
        return None;
    }
    let s = first.named_child(0)?;
    if s.kind() != "string" {
        return None;
    }
    text(s, src)
        .trim_matches(|c| c == '"' || c == '\'')
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

type Collect = fn(Node, &str, &mut Vec<Symbol>);

pub fn outline(lang: Lang, path: &str, src: &str) -> Result<FileOutline, IngestError> {
    let (language, collect): (Language, Collect) = match lang {
        Lang::Python => return outline_python(path, src),
        Lang::Node if path.ends_with(".ts") => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            |n, s, o| js_collect(n, s, None, o),
        ),
        Lang::Node => (tree_sitter_javascript::LANGUAGE.into(), |n, s, o| {
            js_collect(n, s, None, o)
        }),
        Lang::Php => (tree_sitter_php::LANGUAGE_PHP.into(), |n, s, o| {
            php_collect(n, s, None, o)
        }),
        Lang::Rust => (tree_sitter_rust::LANGUAGE.into(), |n, s, o| {
            rust_collect(n, s, None, false, o)
        }),
        Lang::Go => (tree_sitter_go::LANGUAGE.into(), go_collect),
    };
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .map_err(|e| IngestError::Invalid(e.to_string()))?;
    let tree = parser
        .parse(src, None)
        .ok_or_else(|| IngestError::Invalid(format!("{path}: parse failed")))?;
    let mut symbols = Vec::new();
    collect(tree.root_node(), src, &mut symbols);
    Ok(FileOutline {
        path: path.to_string(),
        symbols,
    })
}

/// Collapse whitespace and cap the length so one symbol stays one readable line.
fn squash(s: &str, cap: usize) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= cap {
        one
    } else {
        format!("{}...", one.chars().take(cap).collect::<String>())
    }
}

/// Declaration up to its body: the signature of a function, method or class.
fn head(node: Node, src: &str) -> String {
    let end = node
        .child_by_field_name("body")
        .map_or(node.end_byte(), |b| b.start_byte());
    squash(&src[node.start_byte()..end], 240)
}

fn field_text<'a>(node: Node, field: &str, src: &'a str) -> Option<&'a str> {
    node.child_by_field_name(field).map(|n| text(n, src))
}

/// First line of a doc comment directly above the node (attributes in between are skipped).
fn leading_doc(node: Node, src: &str) -> Option<String> {
    let mut prev = node.prev_named_sibling()?;
    while matches!(
        prev.kind(),
        "attribute_item" | "attribute_list" | "decorator"
    ) {
        prev = prev.prev_named_sibling()?;
    }
    if !prev.kind().contains("comment") {
        return None;
    }
    text(prev, src)
        .lines()
        .map(|l| {
            l.trim()
                .trim_end_matches("*/")
                .trim_start_matches(['/', '*', '#', '!'])
                .trim()
                .to_string()
        })
        .find(|l| !l.is_empty() && !l.starts_with('@'))
}

fn symbol(
    kind: Kind,
    name: &str,
    signature: String,
    doc: Option<String>,
    parent: Option<&str>,
) -> Symbol {
    Symbol {
        kind,
        name: name.to_string(),
        signature,
        doc,
        parent: parent.map(str::to_string),
    }
}

fn js_collect(node: Node, src: &str, parent: Option<&str>, out: &mut Vec<Symbol>) {
    let mut cur = node.walk();
    for child in node.named_children(&mut cur) {
        let def = if child.kind() == "export_statement" {
            match child.child_by_field_name("declaration") {
                Some(d) => d,
                None => continue,
            }
        } else {
            child
        };
        let doc = leading_doc(child, src);
        let name = field_text(def, "name", src).unwrap_or("");
        match def.kind() {
            "class_declaration" | "abstract_class_declaration"
                if parent.is_none() && !name.starts_with('_') =>
            {
                out.push(symbol(Kind::Class, name, head(def, src), doc, None));
                if let Some(body) = def.child_by_field_name("body") {
                    js_collect(body, src, Some(name), out);
                }
            }
            "interface_declaration" | "type_alias_declaration" | "enum_declaration"
                if parent.is_none() && !name.starts_with('_') =>
            {
                out.push(symbol(
                    Kind::Class,
                    name,
                    squash(text(def, src), 500),
                    doc,
                    None,
                ));
            }
            "function_declaration" | "generator_function_declaration"
                if parent.is_none() && !name.starts_with('_') =>
            {
                out.push(symbol(Kind::Function, name, head(def, src), doc, None));
            }
            "lexical_declaration" if parent.is_none() => {
                let mut c = def.walk();
                for d in def
                    .named_children(&mut c)
                    .filter(|d| d.kind() == "variable_declarator")
                {
                    let (Some(n), Some(v)) =
                        (field_text(d, "name", src), d.child_by_field_name("value"))
                    else {
                        continue;
                    };
                    if matches!(
                        v.kind(),
                        "arrow_function" | "function_expression" | "function"
                    ) && !n.starts_with('_')
                    {
                        out.push(symbol(
                            Kind::Function,
                            n,
                            format!("const {n} = {}", head(v, src)),
                            doc.clone(),
                            None,
                        ));
                    }
                }
            }
            "method_definition" if parent.is_some() => {
                let mut c = def.walk();
                let hidden = def.named_children(&mut c).any(|m| {
                    m.kind() == "accessibility_modifier"
                        && matches!(text(m, src), "private" | "protected")
                });
                if !hidden && !name.starts_with('_') && !name.starts_with('#') {
                    out.push(symbol(Kind::Method, name, head(def, src), doc, parent));
                }
            }
            _ => {}
        }
    }
}

fn is_pub(node: Node, src: &str) -> bool {
    let mut c = node.walk();
    let found = node
        .children(&mut c)
        .any(|m| m.kind() == "visibility_modifier" && text(m, src) == "pub");
    found
}

fn rust_collect(
    node: Node,
    src: &str,
    impl_of: Option<&str>,
    trait_impl: bool,
    out: &mut Vec<Symbol>,
) {
    let mut cur = node.walk();
    for child in node.named_children(&mut cur) {
        let name = field_text(child, "name", src).unwrap_or("");
        match child.kind() {
            "function_item" if is_pub(child, src) || (impl_of.is_some() && trait_impl) => {
                let kind = if impl_of.is_some() {
                    Kind::Method
                } else {
                    Kind::Function
                };
                out.push(symbol(
                    kind,
                    name,
                    head(child, src),
                    leading_doc(child, src),
                    impl_of,
                ));
            }
            "struct_item" | "enum_item" | "trait_item" | "type_item" | "union_item"
                if impl_of.is_none() && is_pub(child, src) =>
            {
                out.push(symbol(
                    Kind::Class,
                    name,
                    squash(text(child, src), 600),
                    leading_doc(child, src),
                    None,
                ));
            }
            "impl_item" => {
                let ty = field_text(child, "type", src).unwrap_or("");
                let is_trait = child.child_by_field_name("trait").is_some();
                if let Some(body) = child.child_by_field_name("body") {
                    rust_collect(body, src, Some(ty), is_trait, out);
                }
            }
            // Items declared `pub` in a private module are usually re-exported, so look inside.
            "mod_item" if name != "tests" => {
                if let Some(body) = child.child_by_field_name("body") {
                    rust_collect(body, src, None, false, out);
                }
            }
            _ => {}
        }
    }
}

fn go_exported(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_uppercase())
}

fn go_collect(root: Node, src: &str, out: &mut Vec<Symbol>) {
    let mut cur = root.walk();
    for child in root.named_children(&mut cur) {
        match child.kind() {
            "function_declaration" => {
                let name = field_text(child, "name", src).unwrap_or("");
                if go_exported(name) {
                    out.push(symbol(
                        Kind::Function,
                        name,
                        head(child, src),
                        leading_doc(child, src),
                        None,
                    ));
                }
            }
            "method_declaration" => {
                let name = field_text(child, "name", src).unwrap_or("");
                let recv = field_text(child, "receiver", src)
                    .and_then(|r| {
                        r.trim_matches(|c| c == '(' || c == ')')
                            .split_whitespace()
                            .last()
                    })
                    .map(|t| t.trim_start_matches('*').to_string());
                if go_exported(name) {
                    out.push(symbol(
                        Kind::Method,
                        name,
                        head(child, src),
                        leading_doc(child, src),
                        recv.as_deref(),
                    ));
                }
            }
            "type_declaration" => {
                let mut c = child.walk();
                for spec in child
                    .named_children(&mut c)
                    .filter(|s| s.kind() == "type_spec")
                {
                    let name = field_text(spec, "name", src).unwrap_or("");
                    if go_exported(name) {
                        out.push(symbol(
                            Kind::Class,
                            name,
                            format!("type {}", squash(text(spec, src), 600)),
                            leading_doc(child, src),
                            None,
                        ));
                    }
                }
            }
            _ => {}
        }
    }
}

fn php_collect(node: Node, src: &str, parent: Option<&str>, out: &mut Vec<Symbol>) {
    let mut cur = node.walk();
    for child in node.named_children(&mut cur) {
        let name = field_text(child, "name", src).unwrap_or("");
        match child.kind() {
            "namespace_definition" => {
                if let Some(body) = child.child_by_field_name("body") {
                    php_collect(body, src, None, out);
                }
            }
            "class_declaration"
            | "interface_declaration"
            | "trait_declaration"
            | "enum_declaration"
                if parent.is_none() =>
            {
                out.push(symbol(
                    Kind::Class,
                    name,
                    head(child, src),
                    leading_doc(child, src),
                    None,
                ));
                if let Some(body) = child.child_by_field_name("body") {
                    php_collect(body, src, Some(name), out);
                }
            }
            "function_definition" if parent.is_none() => {
                out.push(symbol(
                    Kind::Function,
                    name,
                    head(child, src),
                    leading_doc(child, src),
                    None,
                ));
            }
            "method_declaration" if parent.is_some() => {
                let mut c = child.walk();
                let hidden = child.named_children(&mut c).any(|m| {
                    m.kind() == "visibility_modifier"
                        && matches!(text(m, src), "private" | "protected")
                });
                if !hidden && (!name.starts_with("__") || name == "__construct") {
                    out.push(symbol(
                        Kind::Method,
                        name,
                        head(child, src),
                        leading_doc(child, src),
                        parent,
                    ));
                }
            }
            _ => {}
        }
    }
}

pub fn render_outline(files: &[FileOutline]) -> String {
    let mut out = String::new();
    for f in files.iter().filter(|f| !f.symbols.is_empty()) {
        out.push_str(&format!("# {}\n", f.path));
        for s in &f.symbols {
            let indent = if s.kind == Kind::Method { "  " } else { "" };
            match &s.doc {
                Some(d) => out.push_str(&format!("{indent}{}  -- {d}\n", s.signature)),
                None => out.push_str(&format!("{indent}{}\n", s.signature)),
            }
        }
        out.push('\n');
    }
    out
}

/// Names of public symbols, excluding constructors and dunders.
pub fn public_names(files: &[FileOutline]) -> Vec<String> {
    let mut names: Vec<String> = files
        .iter()
        .flat_map(|f| &f.symbols)
        .map(|s| s.name.clone())
        .filter(|n| !n.starts_with("__"))
        .collect();
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"
class Client(Base):
    """HTTP client.

    More detail."""
    def __init__(self, api_key: str, timeout=30):
        self._k = api_key

    def get_user(self, user_id: str) -> "User":
        '''Fetch one user.'''
        return self._get(user_id)

    def _get(self, x):
        pass

    class Inner:
        def hidden(self): pass

class _Private:
    def visible_but_hidden_parent(self): pass

@decorator
async def list_all(client, limit=10):
    pass
"#;

    #[test]
    fn extracts_public_surface() {
        let o = outline_python("c.py", SRC).unwrap();
        let sigs: Vec<&str> = o.symbols.iter().map(|s| s.signature.as_str()).collect();
        assert_eq!(
            sigs,
            [
                "class Client(Base)",
                "def __init__(self, api_key: str, timeout=30)",
                "def get_user(self, user_id: str) -> \"User\"",
                "async def list_all(client, limit=10)",
            ]
        );
        assert_eq!(o.symbols[0].doc.as_deref(), Some("HTTP client."));
        assert_eq!(o.symbols[2].doc.as_deref(), Some("Fetch one user."));
        assert_eq!(o.symbols[2].parent.as_deref(), Some("Client"));
        assert_eq!(public_names(&[o]), ["Client", "get_user", "list_all"]);
    }

    fn sigs(lang: Lang, path: &str, src: &str) -> Vec<String> {
        outline(lang, path, src)
            .unwrap()
            .symbols
            .iter()
            .map(|s| s.signature.clone())
            .collect()
    }

    #[test]
    fn javascript_surface() {
        let src = "/** HTTP client. */\nexport class Client extends Base {\n  constructor(key) { super(); }\n  /** Fetch a user. */\n  async getUser(id) { return 1; }\n  _internal() {}\n  #hidden() {}\n}\nexport function listAll(client, limit = 10) {}\nexport const makeClient = (opts) => new Client(opts);\nconst _skip = () => 1;\n";
        let o = outline(Lang::Node, "c.js", src).unwrap();
        let sigs: Vec<&str> = o.symbols.iter().map(|s| s.signature.as_str()).collect();
        assert_eq!(
            sigs,
            [
                "class Client extends Base",
                "constructor(key)",
                "async getUser(id)",
                "function listAll(client, limit = 10)",
                "const makeClient = (opts) =>"
            ]
        );
        assert_eq!(o.symbols[0].doc.as_deref(), Some("HTTP client."));
        assert_eq!(o.symbols[2].parent.as_deref(), Some("Client"));
    }

    #[test]
    fn typescript_hides_private_members() {
        let src = "export interface User { id: string }\nexport class Api {\n  private token: string;\n  private sign(x: string): string { return x; }\n  public get(id: string): Promise<User> { return fetch(id); }\n}\n";
        let sigs = sigs(Lang::Node, "a.ts", src);
        assert_eq!(
            sigs,
            [
                "interface User { id: string }",
                "class Api",
                "public get(id: string): Promise<User>"
            ]
        );
    }

    #[test]
    fn rust_surface() {
        let src = "/// The client.\n#[derive(Debug)]\npub struct Client { pub base: String, key: String }\n\nimpl Client {\n    /// Build one.\n    pub fn new(base: &str) -> Self { todo!() }\n    fn sign(&self) {}\n    pub fn get(&self, id: &str) -> Result<User, Error> { todo!() }\n}\n\nimpl Default for Client {\n    fn default() -> Self { todo!() }\n}\n\npub fn helper() {}\nfn private_fn() {}\n\n#[cfg(test)]\nmod tests {\n    pub fn in_tests() {}\n}\n";
        let o = outline(Lang::Rust, "lib.rs", src).unwrap();
        let sigs: Vec<&str> = o.symbols.iter().map(|s| s.signature.as_str()).collect();
        assert_eq!(
            sigs,
            [
                "pub struct Client { pub base: String, key: String }",
                "pub fn new(base: &str) -> Self",
                "pub fn get(&self, id: &str) -> Result<User, Error>",
                "fn default() -> Self",
                "pub fn helper()"
            ]
        );
        assert_eq!(o.symbols[0].doc.as_deref(), Some("The client."));
        assert_eq!(o.symbols[1].parent.as_deref(), Some("Client"));
    }

    #[test]
    fn go_surface() {
        let src = "package sdk\n\n// Client talks to the API.\ntype Client struct {\n\tBaseURL string\n}\n\n// GetPet fetches a pet.\nfunc (c *Client) GetPet(ctx context.Context, id string) (*Pet, error) { return nil, nil }\n\nfunc (c *Client) do() {}\n\nfunc NewClient(base string) *Client { return nil }\n\nfunc helper() {}\n";
        let o = outline(Lang::Go, "client.go", src).unwrap();
        let sigs: Vec<&str> = o.symbols.iter().map(|s| s.signature.as_str()).collect();
        assert_eq!(
            sigs,
            [
                "type Client struct { BaseURL string }",
                "func (c *Client) GetPet(ctx context.Context, id string) (*Pet, error)",
                "func NewClient(base string) *Client"
            ]
        );
        assert_eq!(o.symbols[1].parent.as_deref(), Some("Client"));
        assert_eq!(o.symbols[1].doc.as_deref(), Some("GetPet fetches a pet."));
    }

    #[test]
    fn php_surface() {
        let src = "<?php\nnamespace Acme;\n\n/** HTTP client. */\nclass Client extends Base {\n    public function __construct(string $key) {}\n    public function getUser(string $id): User { return new User(); }\n    private function sign() {}\n    protected function hook() {}\n    function implicitPublic() {}\n}\n\nfunction helper(int $x): int { return $x; }\n";
        let o = outline(Lang::Php, "Client.php", src).unwrap();
        let sigs: Vec<&str> = o.symbols.iter().map(|s| s.signature.as_str()).collect();
        assert_eq!(
            sigs,
            [
                "class Client extends Base",
                "public function __construct(string $key)",
                "public function getUser(string $id): User",
                "function implicitPublic()",
                "function helper(int $x): int"
            ]
        );
        assert_eq!(o.symbols[0].doc.as_deref(), Some("HTTP client."));
    }

    #[test]
    fn language_detection_and_file_rules() {
        let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            detect_lang(&f(&["Cargo.toml", "src/lib.rs"])),
            Some(Lang::Rust)
        );
        assert_eq!(detect_lang(&f(&["go.mod", "a.go"])), Some(Lang::Go));
        assert_eq!(detect_lang(&f(&["composer.json"])), Some(Lang::Php));
        assert_eq!(
            detect_lang(&f(&["x/a.ts", "x/b.ts", "c.py"])),
            Some(Lang::Node)
        );
        assert_eq!(detect_lang(&f(&["README.md"])), None);
        assert_eq!(Lang::parse("TypeScript"), Some(Lang::Node));
        assert!(Lang::Go.is_test_file("pets_test.go") && !Lang::Go.is_source_file("pets_test.go"));
        assert!(Lang::Node.is_test_file("src/a.test.js") && Lang::Node.is_test_file("test/a.js"));
        assert!(
            !Lang::Node.is_source_file("node_modules/x/index.js")
                && !Lang::Node.is_source_file("types.d.ts")
        );
        assert!(
            Lang::Php.is_test_file("tests/ClientTest.php")
                && Lang::Php.is_source_file("src/Client.php")
        );
        assert!(
            Lang::Rust.is_test_file("tests/api.rs") && Lang::Rust.is_source_file("src/client.rs")
        );
        assert!(
            !Lang::Rust.is_source_file("target/debug/x.rs")
                && !Lang::Python.is_source_file("setup.py")
        );
    }
}
