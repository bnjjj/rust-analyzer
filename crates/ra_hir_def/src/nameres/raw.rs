//! Lowers syntax tree of a rust file into a raw representation of containing
//! items, *without* attaching them to a module structure.
//!
//! That is, raw items don't have semantics, just as syntax, but, unlike syntax,
//! they don't change with trivial source code edits, making them a great tool
//! for building salsa recomputation firewalls.

use std::{ops::Index, sync::Arc};

use hir_expand::{
    ast_id_map::AstIdMap,
    hygiene::Hygiene,
    name::{AsName, Name},
};
use ra_arena::{Arena, Idx};
use ra_prof::profile;
use ra_syntax::{
    ast::{self, AttrsOwner, NameOwner, VisibilityOwner},
    AstNode,
};
use test_utils::tested_by;

use crate::{
    attr::Attrs,
    db::DefDatabase,
    path::{ImportAlias, ModPath},
    visibility::RawVisibility,
    FileAstId, HirFileId, InFile,
};

/// `RawItems` is a set of top-level items in a file (except for impls).
///
/// It is the input to name resolution algorithm. `RawItems` are not invalidated
/// on most edits.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RawItems {
    modules: Arena<ModuleData>,
    imports: Arena<ImportData>,
    defs: Arena<DefData>,
    macros: Arena<MacroData>,
    impls: Arena<ImplData>,
    /// items for top-level module
    items: Vec<RawItem>,
}

impl RawItems {
    pub(crate) fn raw_items_query(db: &dyn DefDatabase, file_id: HirFileId) -> Arc<RawItems> {
        let _p = profile("raw_items_query");
        let mut collector = RawItemsCollector {
            raw_items: RawItems::default(),
            source_ast_id_map: db.ast_id_map(file_id),
            file_id,
            hygiene: Hygiene::new(db.upcast(), file_id),
        };
        if let Some(node) = db.parse_or_expand(file_id) {
            if let Some(source_file) = ast::SourceFile::cast(node.clone()) {
                collector.process_module(None, source_file);
            } else if let Some(item_list) = ast::MacroItems::cast(node) {
                collector.process_module(None, item_list);
            }
        }
        let raw_items = collector.raw_items;
        Arc::new(raw_items)
    }

    pub(super) fn items(&self) -> &[RawItem] {
        &self.items
    }
}

impl Index<Idx<ModuleData>> for RawItems {
    type Output = ModuleData;
    fn index(&self, idx: Idx<ModuleData>) -> &ModuleData {
        &self.modules[idx]
    }
}

impl Index<Import> for RawItems {
    type Output = ImportData;
    fn index(&self, idx: Import) -> &ImportData {
        &self.imports[idx]
    }
}

impl Index<Idx<DefData>> for RawItems {
    type Output = DefData;
    fn index(&self, idx: Idx<DefData>) -> &DefData {
        &self.defs[idx]
    }
}

impl Index<Idx<MacroData>> for RawItems {
    type Output = MacroData;
    fn index(&self, idx: Idx<MacroData>) -> &MacroData {
        &self.macros[idx]
    }
}

impl Index<Idx<ImplData>> for RawItems {
    type Output = ImplData;
    fn index(&self, idx: Idx<ImplData>) -> &ImplData {
        &self.impls[idx]
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub(super) struct RawItem {
    pub(super) attrs: Attrs,
    pub(super) kind: RawItemKind,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(super) enum RawItemKind {
    Module(Idx<ModuleData>),
    Import(Import),
    Def(Idx<DefData>),
    Macro(Idx<MacroData>),
    Impl(Idx<ImplData>),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ModuleData {
    Declaration {
        name: Name,
        visibility: RawVisibility,
        ast_id: FileAstId<ast::Module>,
    },
    Definition {
        name: Name,
        visibility: RawVisibility,
        ast_id: FileAstId<ast::Module>,
        items: Vec<RawItem>,
    },
}

pub(crate) type Import = Idx<ImportData>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportData {
    pub(super) path: ModPath,
    pub(super) alias: Option<ImportAlias>,
    pub(super) is_glob: bool,
    pub(super) is_prelude: bool,
    pub(super) is_extern_crate: bool,
    pub(super) is_macro_use: bool,
    pub(super) visibility: RawVisibility,
}

// type Def = Idx<DefData>;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct DefData {
    pub(super) name: Name,
    pub(super) kind: DefKind,
    pub(super) visibility: RawVisibility,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(super) enum StructDefKind {
    Record,
    Tuple,
    Unit,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(super) enum DefKind {
    Function(FileAstId<ast::FnDef>),
    Struct(FileAstId<ast::StructDef>, StructDefKind),
    Union(FileAstId<ast::UnionDef>),
    Enum(FileAstId<ast::EnumDef>),
    Const(FileAstId<ast::ConstDef>),
    Static(FileAstId<ast::StaticDef>),
    Trait(FileAstId<ast::TraitDef>),
    TypeAlias(FileAstId<ast::TypeAliasDef>),
}

impl DefKind {
    pub fn ast_id(&self) -> FileAstId<ast::ModuleItem> {
        match self {
            DefKind::Function(it) => it.upcast(),
            DefKind::Struct(it, _) => it.upcast(),
            DefKind::Union(it) => it.upcast(),
            DefKind::Enum(it) => it.upcast(),
            DefKind::Const(it) => it.upcast(),
            DefKind::Static(it) => it.upcast(),
            DefKind::Trait(it) => it.upcast(),
            DefKind::TypeAlias(it) => it.upcast(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct MacroData {
    pub(super) ast_id: FileAstId<ast::MacroCall>,
    pub(super) path: ModPath,
    pub(super) name: Option<Name>,
    pub(super) export: bool,
    pub(super) local_inner: bool,
    pub(super) builtin: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ImplData {
    pub(super) ast_id: FileAstId<ast::ImplDef>,
}

struct RawItemsCollector {
    raw_items: RawItems,
    source_ast_id_map: Arc<AstIdMap>,
    file_id: HirFileId,
    hygiene: Hygiene,
}

impl RawItemsCollector {
    fn process_module(
        &mut self,
        current_module: Option<Idx<ModuleData>>,
        body: impl ast::ModuleItemOwner,
    ) {
        for item in body.items() {
            self.add_item(current_module, item)
        }
    }

    fn add_item(&mut self, current_module: Option<Idx<ModuleData>>, item: ast::ModuleItem) {
        let attrs = self.parse_attrs(&item);
        let visibility = RawVisibility::from_ast_with_hygiene(item.visibility(), &self.hygiene);
        let (kind, name) = match item {
            ast::ModuleItem::Module(module) => {
                self.add_module(current_module, module);
                return;
            }
            ast::ModuleItem::UseItem(use_item) => {
                self.add_use_item(current_module, use_item);
                return;
            }
            ast::ModuleItem::ExternCrateItem(extern_crate) => {
                self.add_extern_crate_item(current_module, extern_crate);
                return;
            }
            ast::ModuleItem::ImplDef(it) => {
                self.add_impl(current_module, it);
                return;
            }
            ast::ModuleItem::StructDef(it) => {
                let kind = match it.kind() {
                    ast::StructKind::Record(_) => StructDefKind::Record,
                    ast::StructKind::Tuple(_) => StructDefKind::Tuple,
                    ast::StructKind::Unit => StructDefKind::Unit,
                };
                let id = self.source_ast_id_map.ast_id(&it);
                let name = it.name();
                (DefKind::Struct(id, kind), name)
            }
            ast::ModuleItem::UnionDef(it) => {
                let id = self.source_ast_id_map.ast_id(&it);
                let name = it.name();
                (DefKind::Union(id), name)
            }
            ast::ModuleItem::EnumDef(it) => {
                (DefKind::Enum(self.source_ast_id_map.ast_id(&it)), it.name())
            }
            ast::ModuleItem::FnDef(it) => {
                (DefKind::Function(self.source_ast_id_map.ast_id(&it)), it.name())
            }
            ast::ModuleItem::TraitDef(it) => {
                (DefKind::Trait(self.source_ast_id_map.ast_id(&it)), it.name())
            }
            ast::ModuleItem::TypeAliasDef(it) => {
                (DefKind::TypeAlias(self.source_ast_id_map.ast_id(&it)), it.name())
            }
            ast::ModuleItem::ConstDef(it) => {
                (DefKind::Const(self.source_ast_id_map.ast_id(&it)), it.name())
            }
            ast::ModuleItem::StaticDef(it) => {
                (DefKind::Static(self.source_ast_id_map.ast_id(&it)), it.name())
            }
            ast::ModuleItem::MacroCall(it) => {
                self.add_macro(current_module, it);
                return;
            }
            ast::ModuleItem::ExternBlock(it) => {
                self.add_extern_block(current_module, it);
                return;
            }
        };
        if let Some(name) = name {
            let name = name.as_name();
            let def = self.raw_items.defs.alloc(DefData { name, kind, visibility });
            self.push_item(current_module, attrs, RawItemKind::Def(def));
        }
    }

    fn add_extern_block(
        &mut self,
        current_module: Option<Idx<ModuleData>>,
        block: ast::ExternBlock,
    ) {
        if let Some(items) = block.extern_item_list() {
            for item in items.extern_items() {
                let attrs = self.parse_attrs(&item);
                let visibility =
                    RawVisibility::from_ast_with_hygiene(item.visibility(), &self.hygiene);
                let (kind, name) = match item {
                    ast::ExternItem::FnDef(it) => {
                        (DefKind::Function(self.source_ast_id_map.ast_id(&it)), it.name())
                    }
                    ast::ExternItem::StaticDef(it) => {
                        (DefKind::Static(self.source_ast_id_map.ast_id(&it)), it.name())
                    }
                };

                if let Some(name) = name {
                    let name = name.as_name();
                    let def = self.raw_items.defs.alloc(DefData { name, kind, visibility });
                    self.push_item(current_module, attrs, RawItemKind::Def(def));
                }
            }
        }
    }

    fn add_module(&mut self, current_module: Option<Idx<ModuleData>>, module: ast::Module) {
        let name = match module.name() {
            Some(it) => it.as_name(),
            None => return,
        };
        let attrs = self.parse_attrs(&module);
        let visibility = RawVisibility::from_ast_with_hygiene(module.visibility(), &self.hygiene);

        let ast_id = self.source_ast_id_map.ast_id(&module);
        if module.semicolon_token().is_some() {
            let item =
                self.raw_items.modules.alloc(ModuleData::Declaration { name, visibility, ast_id });
            self.push_item(current_module, attrs, RawItemKind::Module(item));
            return;
        }

        if let Some(item_list) = module.item_list() {
            let item = self.raw_items.modules.alloc(ModuleData::Definition {
                name,
                visibility,
                ast_id,
                items: Vec::new(),
            });
            self.process_module(Some(item), item_list);
            self.push_item(current_module, attrs, RawItemKind::Module(item));
            return;
        }
        tested_by!(name_res_works_for_broken_modules);
    }

    fn add_use_item(&mut self, current_module: Option<Idx<ModuleData>>, use_item: ast::UseItem) {
        // FIXME: cfg_attr
        let is_prelude = use_item.has_atom_attr("prelude_import");
        let attrs = self.parse_attrs(&use_item);
        let visibility = RawVisibility::from_ast_with_hygiene(use_item.visibility(), &self.hygiene);

        let mut buf = Vec::new();
        ModPath::expand_use_item(
            InFile { value: use_item, file_id: self.file_id },
            &self.hygiene,
            |path, _use_tree, is_glob, alias| {
                let import_data = ImportData {
                    path,
                    alias,
                    is_glob,
                    is_prelude,
                    is_extern_crate: false,
                    is_macro_use: false,
                    visibility: visibility.clone(),
                };
                buf.push(import_data);
            },
        );
        for import_data in buf {
            self.push_import(current_module, attrs.clone(), import_data);
        }
    }

    fn add_extern_crate_item(
        &mut self,
        current_module: Option<Idx<ModuleData>>,
        extern_crate: ast::ExternCrateItem,
    ) {
        if let Some(name_ref) = extern_crate.name_ref() {
            let path = ModPath::from_name_ref(&name_ref);
            let visibility =
                RawVisibility::from_ast_with_hygiene(extern_crate.visibility(), &self.hygiene);
            let alias = extern_crate.alias().map(|a| {
                a.name().map(|it| it.as_name()).map_or(ImportAlias::Underscore, ImportAlias::Alias)
            });
            let attrs = self.parse_attrs(&extern_crate);
            // FIXME: cfg_attr
            let is_macro_use = extern_crate.has_atom_attr("macro_use");
            let import_data = ImportData {
                path,
                alias,
                is_glob: false,
                is_prelude: false,
                is_extern_crate: true,
                is_macro_use,
                visibility,
            };
            self.push_import(current_module, attrs, import_data);
        }
    }

    fn add_macro(&mut self, current_module: Option<Idx<ModuleData>>, m: ast::MacroCall) {
        let attrs = self.parse_attrs(&m);
        let path = match m.path().and_then(|path| ModPath::from_src(path, &self.hygiene)) {
            Some(it) => it,
            _ => return,
        };

        let name = m.name().map(|it| it.as_name());
        let ast_id = self.source_ast_id_map.ast_id(&m);

        // FIXME: cfg_attr
        let export_attr = attrs.by_key("macro_export");

        let export = export_attr.exists();
        let local_inner = if export {
            export_attr.tt_values().map(|it| &it.token_trees).flatten().any(|it| match it {
                tt::TokenTree::Leaf(tt::Leaf::Ident(ident)) => {
                    ident.text.contains("local_inner_macros")
                }
                _ => false,
            })
        } else {
            false
        };

        let builtin = attrs.by_key("rustc_builtin_macro").exists();

        let m = self.raw_items.macros.alloc(MacroData {
            ast_id,
            path,
            name,
            export,
            local_inner,
            builtin,
        });
        self.push_item(current_module, attrs, RawItemKind::Macro(m));
    }

    fn add_impl(&mut self, current_module: Option<Idx<ModuleData>>, imp: ast::ImplDef) {
        let attrs = self.parse_attrs(&imp);
        let ast_id = self.source_ast_id_map.ast_id(&imp);
        let imp = self.raw_items.impls.alloc(ImplData { ast_id });
        self.push_item(current_module, attrs, RawItemKind::Impl(imp))
    }

    fn push_import(
        &mut self,
        current_module: Option<Idx<ModuleData>>,
        attrs: Attrs,
        data: ImportData,
    ) {
        let import = self.raw_items.imports.alloc(data);
        self.push_item(current_module, attrs, RawItemKind::Import(import))
    }

    fn push_item(
        &mut self,
        current_module: Option<Idx<ModuleData>>,
        attrs: Attrs,
        kind: RawItemKind,
    ) {
        match current_module {
            Some(module) => match &mut self.raw_items.modules[module] {
                ModuleData::Definition { items, .. } => items,
                ModuleData::Declaration { .. } => unreachable!(),
            },
            None => &mut self.raw_items.items,
        }
        .push(RawItem { attrs, kind })
    }

    fn parse_attrs(&self, item: &impl ast::AttrsOwner) -> Attrs {
        Attrs::new(item, &self.hygiene)
    }
}
