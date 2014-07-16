#![feature(plugin_registrar)]

extern crate rustc;
extern crate syntax;

use std::gc::Gc;
use std::io::Command;
use std::str;

use rustc::plugin::Registry;
use syntax::abi;
use syntax::ast;
use syntax::attr;
use syntax::codemap::Span;
use syntax::ext::base::{ExtCtxt, DummyResult, MacroDef};
use syntax::ext::base::MacResult;
use syntax::ext::build::AstBuilder;
use syntax::parse::parser::Parser;
use syntax::parse::token;
use syntax::parse::token::{special_idents, InternedString};
use syntax::print::pprust;
use syntax::util::small_vector::SmallVector;

struct LibInfo {
    lib: String,
    statik: bool,
    deps: Vec<(String, bool)>, // (name, static)
    locs: Vec<String>,
}

struct MacItems {
    items: Vec<Gc<ast::Item>>,
}

#[plugin_registrar]
pub fn plugin_registrar(reg: &mut Registry) {
    reg.register_macro("link_config", expand_link_config);
}

fn expand_link_config(ecx: &mut ExtCtxt, span: Span,
                      tts: &[ast::TokenTree]) -> Box<MacResult> {
    let mut parser = ecx.new_parser_from_tts(tts);
    let (pkg, sp) = match parse_string(ecx, &mut parser) {
        Some(s) => s,
        None => return DummyResult::any(span),
    };
    if !parser.eat(&token::EOF) {
        ecx.span_err(parser.span, "only one string literal allowed");
        return DummyResult::any(span);
    }

    let dyn = match system_pkgconfig(ecx, sp, pkg.as_slice(), false) {
        Some(info) => info,
        None => return DummyResult::any(span),
    };
    let statik = match system_pkgconfig(ecx, sp, pkg.as_slice(), true) {
        Some(info) => info,
        None => return DummyResult::any(span),
    };

    let dyn = block(ecx, sp, &dyn);
    let statik = block(ecx, sp, &statik);

    box MacItems { items: vec![dyn, statik] } as Box<MacResult>
}

fn system_pkgconfig(ecx: &mut ExtCtxt, sp: Span, pkg: &str,
                    statik: bool) -> Option<LibInfo> {

    let mut cmd = Command::new("pkg-config");
    cmd.arg(pkg).arg("--libs").env("PKG_CONFIG_ALLOW_SYSTEM_LIBS", "1");
    if statik {
        cmd.arg("--static");
    }
    let out = match cmd.output() {
        Ok(out) => out,
        Err(e) => {
            ecx.span_err(sp, format!("could not run pkg-config: {}", e).as_slice());
            return None
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
        return None
    }

    let mut libs = Vec::new();
    let mut locs = Vec::new();
    for arg in stdout.split(' ').filter(|l| !l.is_empty()) {
        if arg.starts_with("-l") {
            libs.push(arg.slice_from(2));
        } else if arg.starts_with("-L") {
            locs.push(arg.slice_from(2).to_string());
        }
    }

    let libs = libs.move_iter().map(|lib| {
        (lib.to_string(), statik && locs.iter().any(|l| {
            let base = Path::new(l.as_slice());
            base.join(format!("lib{}.a", lib)).exists() ||
                base.join(format!("{}.lib", lib)).exists() ||
                base.join(format!("lib{}.lib", lib)).exists()
        }))
    }).collect();
    Some(LibInfo {
        lib: pkg.to_string(),
        deps: libs,
        locs: locs,
        statik: statik,
    })
}

fn block(ecx: &mut ExtCtxt, sp: Span, info: &LibInfo) -> Gc<ast::Item> {
    let lib = token::intern_and_get_ident(info.lib.as_slice());
    let cfg = ecx.meta_name_value(sp, InternedString::new("statik"),
                                  ast::LitStr(lib, ast::CookedStr));
    let cfg = ecx.meta_list(sp, InternedString::new("cfg"), vec![if info.statik {
        cfg
    } else {
        ecx.meta_list(sp, InternedString::new("not"), vec![cfg])
    }]);

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
        let l = token::intern_and_get_ident(format!("-L{}", l).as_slice());
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
                parser: &mut Parser) -> Option<(String, Span)> {
    let entry = ecx.expand_expr(parser.parse_expr());
    match entry.node {
        ast::ExprLit(lit) => {
            match lit.node {
                ast::LitStr(ref s, _) => return Some((s.to_string(), entry.span)),
                _ => {}
            }
        }
        _ => {}
    }
    ecx.span_err(entry.span, format!(
        "expected string literal but got `{}`",
        pprust::expr_to_string(entry)).as_slice());
    None
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
