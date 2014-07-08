#![feature(plugin_registrar)]

extern crate rustc;
extern crate syntax;

use std::io::Command;
use std::str;

use rustc::plugin::Registry;
use syntax::ast;
use syntax::abi;
use syntax::codemap::Span;
use syntax::ext::base::{ExtCtxt, DummyResult, MacItem};
use syntax::ext::base::MacResult;
use syntax::ext::build::AstBuilder;
use syntax::parse::parser::Parser;
use syntax::parse::token;
use syntax::parse::token::{special_idents, InternedString};
use syntax::print::pprust;

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

    let out = match Command::new("pkg-config").arg(pkg.as_slice())
                                              .arg("--libs").output() {
        Ok(out) => out,
        Err(e) => {
            ecx.span_err(sp, format!("could not run pkg-config: {}", e).as_slice());
            return DummyResult::any(sp)
        }
    };
    let stdout = str::from_utf8(out.output.as_slice()).unwrap();
    let stderr = str::from_utf8(out.error.as_slice()).unwrap();
    if !out.status.success() {
        ecx.span_err(sp, format!("pkg-config did not exit successfully: {}\n\
                                  --- stdout\n{}\n--- stderr\n{}",
                                 out.status, stdout, stderr).as_slice());
        return DummyResult::any(sp)
    }

    let mut libs = Vec::new();
    let mut locs = Vec::new();
    for arg in stdout.split(' ').filter(|l| !l.is_empty()) {
        if arg.starts_with("-l") {
            libs.push(arg.slice_from(2));
        } else if arg.starts_with("-L") {
            locs.push(arg.slice_from(2));
        }
    }

    let mut attrs = libs.iter().map(|l| {
        // Build #[link(name = <l>)]
        let l = token::intern_and_get_ident(*l);
        let name = ecx.meta_name_value(sp, InternedString::new("name"),
                                       ast::LitStr(l, ast::CookedStr));
        let list = ecx.meta_list(sp, InternedString::new("link"), vec![name]);
        ecx.attribute(sp, list)
    }).chain(locs.iter().map(|l| {
        let l = token::intern_and_get_ident(format!("-L{}", l).as_slice());
        let args = ecx.meta_name_value(sp, InternedString::new("link_args"),
                                       ast::LitStr(l, ast::CookedStr));
        ecx.attribute(sp, args)
    }));

    let block = ecx.item(sp, special_idents::invalid, attrs.collect(),
                         ast::ItemForeignMod(ast::ForeignMod {
        abi: abi::C,
        view_items: Vec::new(),
        items: Vec::new(),
    }));

    return MacItem::new(block)
}

fn parse_string(ecx: &mut ExtCtxt,
                parser: &mut Parser) -> Option<(String, Span)> {
    let entry = ecx.expand_expr(parser.parse_expr());
    match entry.node {
        ast::ExprLit(lit) => {
            match lit.node {
                ast::LitStr(ref s, _) => return Some((s.to_str(), entry.span)),
                _ => {}
            }
        }
        _ => {}
    }
    ecx.span_err(entry.span, format!(
        "expected string literal but got `{}`",
        pprust::expr_to_str(entry)).as_slice());
    None
}
