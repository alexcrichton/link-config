#![feature(plugin_registrar, macro_rules)]

extern crate rustc;
extern crate syntax;

use std::dynamic_lib::DynamicLibrary;
use std::gc::Gc;
use std::io::{fs, Command};
use std::os;
use std::str;
use std::sync::{Once, ONCE_INIT};

use rustc::plugin::Registry;
use syntax::abi;
use syntax::ast;
use syntax::attr;
use syntax::codemap::Span;
use syntax::ext::base::{ExtCtxt, DummyResult, MacroDef};
use syntax::ext::base::MacResult;
use syntax::ext::build::AstBuilder;
use syntax::fold::Folder;
use syntax::parse::parser::Parser;
use syntax::parse::token;
use syntax::parse::token::{special_idents, InternedString};
use syntax::print::pprust;
use syntax::util::small_vector::SmallVector;

struct LibInfo {
    lib: String,
    state: State,
    deps: Vec<(String, bool)>, // (name, static)
    locs: Vec<Path>,
}

struct MacItems {
    items: Vec<Gc<ast::Item>>,
}

enum State { Dynamic, Static(SystemDeps) }
enum SystemDeps { SystemDynamic, SystemStatic }
enum Favor { FavorDynamic, FavorStatic }

#[plugin_registrar]
pub fn plugin_registrar(reg: &mut Registry) {
    reg.register_macro("link_config", expand_link_config);
}

fn expand_link_config(ecx: &mut ExtCtxt, span: Span,
                      tts: &[ast::TokenTree]) -> Box<MacResult> {
    macro_rules! try_dummy( ($e:expr) => (
        match $e { Ok(s) => s, Err(()) => return DummyResult::any(span) }
    ) )

    let mut parser = ecx.new_parser_from_tts(tts);
    let (pkg, sp) = try_dummy!(parse_string(ecx, &mut parser));
    let mut favor_dynamic = FavorDynamic;
    let mut dylib_state = Some(Dynamic);
    let mut static_state = Some(Static(SystemDynamic));
    if parser.eat(&token::COMMA) && parser.eat(&token::LBRACKET) {
        while !parser.eat(&token::RBRACKET) {
            parser.eat(&token::COMMA);
            let (modifier, sp) = try_dummy!(parse_string(ecx, &mut parser));
            match modifier.as_slice() {
                "only_static" => {
                    dylib_state = None;
                    favor_dynamic = FavorStatic;
                }
                "only_dylib" => {
                    static_state = None;
                    favor_dynamic = FavorDynamic;
                }
                "system_static" => static_state = Some(Static(SystemStatic)),
                "favor_static" => favor_dynamic = FavorStatic,
                s => ecx.span_err(sp, format!("unknown modifier: `{}`",
                                              s).as_slice()),
            }
        }
    }
    if !parser.eat(&token::EOF) {
        ecx.span_err(parser.span, "only one string literal allowed");
        return DummyResult::any(span);
    }

    let dyn = try_dummy!(system_pkgconfig(ecx, sp, pkg.as_slice(), dylib_state));
    let statik = try_dummy!(system_pkgconfig(ecx, sp, pkg.as_slice(), static_state));

    let mut items = Vec::new();
    match dyn {
        Some(info) => items.push(block(ecx, sp, &info, favor_dynamic)),
        None => {}
    }
    match statik {
        Some(info) => items.push(block(ecx, sp, &info, favor_dynamic)),
        None => {}
    }
    box MacItems { items: items } as Box<MacResult>
}

fn system_pkgconfig(ecx: &mut ExtCtxt, sp: Span, pkg: &str,
                    state: Option<State>) -> Result<Option<LibInfo>, ()> {
    let state = match state {
        Some(state) => state,
        None => return Ok(None),
    };
    add_cargo_pkg_config_paths();

    let mut cmd = Command::new("pkg-config");
    cmd.arg(pkg).arg("--libs").env("PKG_CONFIG_ALLOW_SYSTEM_LIBS", "1");
    match state {
        Static(..) => { cmd.arg("--static"); }
        Dynamic => {}
    }
    let out = match cmd.output() {
        Ok(out) => out,
        Err(e) => {
            ecx.span_err(sp, format!("could not run pkg-config: {}", e).as_slice());
            return Err(())
        }
    };
    let stdout = str::from_utf8(out.output.as_slice()).unwrap();
    let stderr = str::from_utf8(out.error.as_slice()).unwrap();
    if !out.status.success() {
        let mut msg = format!("pkg-config did not exit successfully: {}",
                              out.status);
        if stdout.len() > 0 {
            msg.push_str("\n--- stdout\n");
            msg.push_str(stdout);
        }
        if stderr.len() > 0 {
            msg.push_str("\n--- stderr\n");
            msg.push_str(stderr);
        }
        ecx.span_err(sp, msg.as_slice());
        return Err(())
    }

    let mut libs = Vec::new();
    let mut locs = Vec::new();
    for arg in stdout.split(' ').filter(|l| !l.is_empty()) {
        if arg.starts_with("-l") {
            libs.push(arg.slice_from(2));
        } else if arg.starts_with("-L") {
            locs.push(Path::new(arg.slice_from(2).to_string()));
        }
    }

    let allow_static = match state {
        Static(..) => true,
        _ => false,
    };
    let allow_static_system = match state {
        Static(SystemStatic) => true,
        _ => false,
    };

    let cargo_locs = cargo_native_dirs();
    let libs = libs.move_iter().map(|lib| {
        let mut candidates = cargo_locs.iter().chain(locs.iter());
        (lib.to_string(), allow_static && candidates.any(|base| {
            (allow_static_system || !base.as_vec().starts_with(b"/usr")) &&
                (base.join(format!("lib{}.a", lib)).exists() ||
                 base.join(format!("{}.lib", lib)).exists() ||
                 base.join(format!("lib{}.lib", lib)).exists())
        }))
    }).collect();
    Ok(Some(LibInfo {
        lib: pkg.to_string(),
        deps: libs,
        locs: locs,
        state: state,
    }))
}

fn block(ecx: &mut ExtCtxt, sp: Span, info: &LibInfo,
         favor: Favor) -> Gc<ast::Item> {
    let lib = token::intern_and_get_ident(info.lib.as_slice());
    let s = match favor {
        FavorDynamic => InternedString::new("statik"),
        FavorStatic => InternedString::new("dylib"),
    };
    let cfg = ecx.meta_name_value(sp, s, ast::LitStr(lib, ast::CookedStr));
    let cfg = match (info.state, favor) {
        (Static(..), FavorDynamic) |
        (Dynamic, FavorStatic) => cfg,
        (Dynamic, FavorDynamic) |
        (Static(..), FavorStatic) => {
            ecx.meta_list(sp, InternedString::new("not"), vec![cfg])
        }
    };
    let cfg = ecx.meta_list(sp, InternedString::new("cfg"), vec![cfg]);

    let attrs = vec![ecx.attribute(sp, cfg)].move_iter();
    let attrs = attrs.chain(info.deps.iter().map(|&(ref l, statik)| {
        // Build #[link(name = <l>)]
        let l = token::intern_and_get_ident(l.as_slice());
        let mut items = Vec::new();
        items.push(ecx.meta_name_value(sp, InternedString::new("name"),
                                       ast::LitStr(l, ast::CookedStr)));
        if statik {
            let l = InternedString::new("static");
            items.push(ecx.meta_name_value(sp, InternedString::new("kind"),
                                           ast::LitStr(l, ast::CookedStr)));
        }
        let list = ecx.meta_list(sp, InternedString::new("link"), items);
        ecx.attribute(sp, list)
    }));
    let attrs = attrs.chain(info.locs.iter().map(|l| {
        let l = token::intern_and_get_ident(format!("-L{}", l.display()).as_slice());
        let args = ecx.meta_name_value(sp, InternedString::new("link_args"),
                                       ast::LitStr(l, ast::CookedStr));
        ecx.attribute(sp, args)
    }));
    let mut attrs = attrs.map(|attr| {
        attr::mark_used(&attr);
        attr
    });

    ecx.item(sp, special_idents::invalid, attrs.collect(),
             ast::ItemForeignMod(ast::ForeignMod {
        abi: abi::C,
        view_items: Vec::new(),
        items: Vec::new(),
    }))
}

fn parse_string(ecx: &mut ExtCtxt,
                parser: &mut Parser) -> Result<(String, Span), ()> {
    let entry = ecx.expander().fold_expr(parser.parse_expr());
    match entry.node {
        ast::ExprLit(lit) => {
            match lit.node {
                ast::LitStr(ref s, _) => return Ok((s.to_string(), entry.span)),
                _ => {}
            }
        }
        _ => {}
    }
    ecx.span_err(entry.span, format!(
        "expected string literal but got `{}`",
        pprust::expr_to_string(&*entry)).as_slice());
    Err(())
}

impl MacResult for MacItems {
    fn make_items(&self) -> Option<SmallVector<Gc<ast::Item>>> {
        Some(self.items.iter().map(|a| a.clone()).collect())
    }
    fn make_stmt(&self) -> Option<Gc<ast::Stmt>> { None }
    fn make_def(&self) -> Option<MacroDef> { None }
    fn make_expr(&self) -> Option<Gc<ast::Expr>> { None }
    fn make_pat(&self) -> Option<Gc<ast::Pat>> { None }
}

// lol hax
fn cargo_native_dirs() -> Vec<Path> {
    DynamicLibrary::search_path().move_iter().filter_map(|path| {
        if !path.ends_with_path(&Path::new("deps")) { return None }
        let native = path.dir_path().join("native");
        fs::readdir(&native).ok()
    }).flat_map(|a| a.move_iter()).collect()
}

fn add_cargo_pkg_config_paths() {
    static mut DONE: Once = ONCE_INIT;
    unsafe { DONE.doit(add_cargo_pkg_config_paths) }

    fn add_cargo_pkg_config_paths() {
        let path = os::getenv_as_bytes("PKG_CONFIG_PATH").unwrap_or(Vec::new());
        let mut pkg_config_path = os::split_paths(path.as_slice());
        pkg_config_path.push_all(cargo_native_dirs().as_slice());
        let pkg_config_path = os::join_paths(pkg_config_path.as_slice())
                                 .unwrap();
        os::setenv("PKG_CONFIG_PATH", pkg_config_path.as_slice());
    }
}
