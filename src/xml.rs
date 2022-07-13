use ctype::isspace;
use nodes::{AstNode, ListType, NodeCode, NodeValue, TableAlignment};
use parser::{ComrakOptions, ComrakPlugins};
use regex::Regex;
use scanners;
use std::borrow::Cow;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::str;
use strings::build_opening_tag;

use crate::nodes::NodeHtmlBlock;

/// Formats an AST as HTML, modified by the given options.
pub fn format_document<'a>(
    root: &'a AstNode<'a>,
    options: &ComrakOptions,
    output: &mut dyn Write,
) -> io::Result<()> {
    format_document_with_plugins(root, &options, output, &ComrakPlugins::default())
}

/// Formats an AST as HTML, modified by the given options. Accepts custom plugins.
pub fn format_document_with_plugins<'a>(
    root: &'a AstNode<'a>,
    options: &ComrakOptions,
    output: &mut dyn Write,
    plugins: &ComrakPlugins,
) -> io::Result<()> {
    output.write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n")?;
    output.write_all(b"<!DOCTYPE document SYSTEM \"CommonMark.dtd\">\n")?;

    XmlFormatter::new(options, output, plugins).format(root, false)
}

struct XmlFormatter<'o> {
    output: &'o mut dyn Write,
    options: &'o ComrakOptions,
    plugins: &'o ComrakPlugins<'o>,
    indent: u32,
}

impl<'o> XmlFormatter<'o> {
    fn new(
        options: &'o ComrakOptions,
        output: &'o mut dyn Write,
        plugins: &'o ComrakPlugins,
    ) -> Self {
        XmlFormatter {
            options,
            output,
            plugins,
            indent: 0,
        }
    }

    fn escape(&mut self, buffer: &[u8]) -> io::Result<()> {
        lazy_static! {
            static ref XML_SAFE: [bool; 256] = {
                let mut a = [true; 256];
                for &c in b"&<>\"".iter() {
                    a[c as usize] = false;
                }
                a
            };
        }
        let mut offset = 0;
        for (i, &byte) in buffer.iter().enumerate() {
            if !XML_SAFE[byte as usize] {
                let esc: &[u8] = match byte {
                    b'"' => b"&quot;",
                    b'&' => b"&amp;",
                    b'<' => b"&lt;",
                    b'>' => b"&gt;",
                    _ => unreachable!(),
                };
                self.output.write_all(&buffer[offset..i])?;
                self.output.write_all(esc)?;
                offset = i + 1;
            }
        }
        self.output.write_all(&buffer[offset..])?;
        Ok(())
    }

    fn format<'a>(&mut self, node: &'a AstNode<'a>, plain: bool) -> io::Result<()> {
        // Traverse the AST iteratively using a work stack, with pre- and
        // post-child-traversal phases. During pre-order traversal render the
        // opening tags, then push the node back onto the stack for the
        // post-order traversal phase, then push the children in reverse order
        // onto the stack and begin rendering first child.

        enum Phase {
            Pre,
            Post,
        }
        let mut stack = vec![(node, plain, Phase::Pre)];

        while let Some((node, plain, phase)) = stack.pop() {
            match phase {
                Phase::Pre => {
                    let new_plain;
                    if plain {
                        match node.data.borrow().value {
                            NodeValue::Text(ref literal)
                            | NodeValue::Code(NodeCode { ref literal, .. })
                            | NodeValue::HtmlInline(ref literal) => {
                                self.escape(literal)?;
                            }
                            NodeValue::LineBreak | NodeValue::SoftBreak => {
                                self.output.write_all(b" ")?;
                            }
                            _ => (),
                        }
                        new_plain = plain;
                    } else {
                        stack.push((node, false, Phase::Post));
                        new_plain = self.format_node(node, true)?;
                    }

                    for ch in node.reverse_children() {
                        stack.push((ch, new_plain, Phase::Pre));
                    }
                }
                Phase::Post => {
                    debug_assert!(!plain);
                    self.format_node(node, false)?;
                }
            }
        }

        Ok(())
    }

    fn collect_text<'a>(&self, node: &'a AstNode<'a>, output: &mut Vec<u8>) {
        match node.data.borrow().value {
            NodeValue::Text(ref literal) | NodeValue::Code(NodeCode { ref literal, .. }) => {
                output.extend_from_slice(literal)
            }
            NodeValue::LineBreak | NodeValue::SoftBreak => output.push(b' '),
            _ => {
                for n in node.children() {
                    self.collect_text(n, output);
                }
            }
        }
    }

    fn indent(&mut self) -> io::Result<()> {
        for _ in 0..self.indent {
            self.output.write_all(b" ")?;
        }
        Ok(())
    }

    fn format_node<'a>(&mut self, node: &'a AstNode<'a>, entering: bool) -> io::Result<bool> {
        if entering {
            self.indent()?;

            let ast = node.data.borrow();

            write!(self.output, "<{}", ast.value.xml_node_name())?;

            if self.options.render.sourcepos && ast.start_line != 0 {
                write!(
                    self.output,
                    " sourcepos=\"{}:{}-{}:{}\"",
                    ast.start_line, ast.start_column, ast.end_line, ast.end_column,
                )?;
            }

            let mut was_literal = false;

            match ast.value {
                NodeValue::Document => self
                    .output
                    .write_all(b" xmlns=\"http://commonmark.org/xml/1.0\"")?,
                NodeValue::Text(ref literal)
                | NodeValue::Code(NodeCode { ref literal, .. })
                | NodeValue::HtmlBlock(NodeHtmlBlock { ref literal, .. })
                | NodeValue::HtmlInline(ref literal) => {
                    self.output.write_all(b" xml:space=\"preserve\">")?;
                    self.escape(literal)?;
                    write!(self.output, "</{}", ast.value.xml_node_name())?;
                    was_literal = true;
                }
                NodeValue::List(ref nl) => {
                    if nl.list_type == ListType::Bullet {
                        self.output.write_all(b" type=\"bullet\"")?;
                    } else {
                        write!(
                            self.output,
                            " type=\"ordered\" start=\"{}\" delim=\"{}\"",
                            nl.start,
                            nl.delimiter.xml_name()
                        )?;
                    }
                    write!(self.output, " tight=\"{}\"", nl.tight)?;
                }
                NodeValue::FrontMatter(_) => (),
                NodeValue::BlockQuote => {}
                NodeValue::Item(..) => {}
                NodeValue::DescriptionList => {}
                NodeValue::DescriptionItem(..) => (),
                NodeValue::DescriptionTerm => {}
                NodeValue::DescriptionDetails => {}
                NodeValue::Heading(ref nch) => {
                    write!(self.output, " level=\"{}\"", nch.level)?;
                }
                NodeValue::CodeBlock(ref ncb) => {
                    if !ncb.info.is_empty() {
                        self.output.write_all(b" info=\"")?;
                        self.output.write_all(&ncb.info)?;
                        self.output.write_all(b"\"")?;
                    }
                    self.output.write_all(b" xml:space=\"preserve\">")?;
                    self.escape(&ncb.literal)?;
                    write!(self.output, "</{}", ast.value.xml_node_name())?;
                    was_literal = true;
                }
                NodeValue::ThematicBreak => {}
                NodeValue::Paragraph => {}
                NodeValue::LineBreak => {}
                NodeValue::SoftBreak => {}
                NodeValue::Strong => {}
                NodeValue::Emph => {}
                NodeValue::Strikethrough => {}
                NodeValue::Superscript => {}
                NodeValue::Link(ref nl) | NodeValue::Image(ref nl) => {
                    self.output.write_all(b" destination=\"")?;
                    self.escape(&nl.url)?;
                    self.output.write_all(b"\" title=\"")?;
                    self.escape(&nl.title)?;
                    self.output.write_all(b"\"")?;
                }
                NodeValue::Table(..) => {
                    // TODO
                    // if entering {
                    //     self.output.write_all(b"<table>\n")?;
                    // } else {
                    //     if !node
                    //         .last_child()
                    //         .unwrap()
                    //         .same_node(node.first_child().unwrap())
                    //     {
                    //         self.output.write_all(b"</tbody>\n")?;
                    //     }
                    //     self.output.write_all(b"</table>\n")?;
                    // }
                }
                NodeValue::TableRow(header) => {
                    // TODO
                    // if entering {
                    //     if header {
                    //         self.output.write_all(b"<thead>\n")?;
                    //     } else if let Some(n) = node.previous_sibling() {
                    //         if let NodeValue::TableRow(true) = n.data.borrow().value {
                    //             self.output.write_all(b"<tbody>\n")?;
                    //         }
                    //     }
                    //     self.output.write_all(b"<tr>")?;
                    // } else {
                    //     self.output.write_all(b"</tr>")?;
                    //     if header {
                    //         self.output.write_all(b"</thead>")?;
                    //     }
                    // }
                }
                NodeValue::TableCell => {
                    // TODO
                    // let row = &node.parent().unwrap().data.borrow().value;
                    // let in_header = match *row {
                    //     NodeValue::TableRow(header) => header,
                    //     _ => panic!(),
                    // };

                    // let table = &node.parent().unwrap().parent().unwrap().data.borrow().value;
                    // let alignments = match *table {
                    //     NodeValue::Table(ref alignments) => alignments,
                    //     _ => panic!(),
                    // };

                    // if entering {
                    //     if in_header {
                    //         self.output.write_all(b"<th")?;
                    //     } else {
                    //         self.output.write_all(b"<td")?;
                    //     }

                    //     let mut start = node.parent().unwrap().first_child().unwrap();
                    //     let mut i = 0;
                    //     while !start.same_node(node) {
                    //         i += 1;
                    //         start = start.next_sibling().unwrap();
                    //     }

                    //     match alignments[i] {
                    //         TableAlignment::Left => {
                    //             self.output.write_all(b" align=\"left\"")?;
                    //         }
                    //         TableAlignment::Right => {
                    //             self.output.write_all(b" align=\"right\"")?;
                    //         }
                    //         TableAlignment::Center => {
                    //             self.output.write_all(b" align=\"center\"")?;
                    //         }
                    //         TableAlignment::None => (),
                    //     }

                    //     self.output.write_all(b">")?;
                    // } else if in_header {
                    //     self.output.write_all(b"</th>")?;
                    // } else {
                    //     self.output.write_all(b"</td>")?;
                    // }
                }
                NodeValue::FootnoteDefinition(_) => {
                    // TODO
                    // if entering {
                    //     if self.footnote_ix == 0 {
                    //         self.output
                    //             .write_all(b"<section class=\"footnotes\">\n<ol>\n")?;
                    //     }
                    //     self.footnote_ix += 1;
                    //     writeln!(self.output, "<li id=\"fn{}\">", self.footnote_ix)?;
                    // } else {
                    //     if self.put_footnote_backref()? {
                    //         self.output.write_all(b"\n")?;
                    //     }
                    //     self.output.write_all(b"</li>\n")?;
                    // }
                }
                NodeValue::FootnoteReference(ref r) => {
                    // TODO
                    // if entering {
                    //     let r = str::from_utf8(r).unwrap();
                    //     write!(
                    //         self.output,
                    //         "<sup class=\"footnote-ref\"><a href=\"#fn{}\" id=\"fnref{}\">{}</a></sup>",
                    //         r, r, r
                    //     )?;
                    // }
                }
                NodeValue::TaskItem(checked) => {
                    // TODO
                    // if entering {
                    //     if checked {
                    //         self.output.write_all(
                    //             b"<input type=\"checkbox\" disabled=\"\" checked=\"\" /> ",
                    //         )?;
                    //     } else {
                    //         self.output
                    //             .write_all(b"<input type=\"checkbox\" disabled=\"\" /> ")?;
                    //     }
                    // }
                }
            }

            if node.first_child().is_some() {
                self.indent += 2;
            } else if !was_literal {
                self.output.write_all(b" /")?;
            }
            self.output.write_all(b">\n")?;
        } else if node.first_child().is_some() {
            self.indent -= 2;
            self.indent()?;
            write!(
                self.output,
                "</{}>\n",
                node.data.borrow().value.xml_node_name()
            )?;
        }
        Ok(false)
    }
}
