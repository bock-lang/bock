#![allow(
    unused_variables,
    unused_imports,
    unused_parens,
    dead_code,
    non_upper_case_globals
)]

#[derive(Clone)]
pub enum MarkdownNode {
    Heading {
        level: i64,
        text: String,
    },
    Paragraph {
        text: String,
    },
    CodeBlock {
        lang: String,
        content: String,
    },
    Bold {
        text: String,
    },
    Italic {
        text: String,
    },
    Link {
        text: String,
        url: String,
    },
    ListItem {
        text: String,
    },
    Text {
        content: String,
    },
}

#[derive(Clone)]
pub struct ParseError {
    pub message: String,
    pub position: i64,
}

pub type ParseResult = Result<MarkdownNode, ParseError>;

fn join_strings(items: Vec<String>, sep: String) -> String {
    let mut result = "".to_string();
    let mut first = true;
    for item in items {
        if first {
            result = item;
            first = false;
        } else {
            result = ((result + sep) + item);
        }
    }
    result
}

pub fn parse_heading(input: String, pos: i64) -> ParseResult {
    if !((input).starts_with(&("#".to_string()) as &str)) {
        return Err(ParseError { message: "not a heading: does not start with #".to_string(), position: pos });
    }
    let mut level = 0_i64;
    if (input).starts_with(&("######".to_string()) as &str) {
        level = 6_i64;
    } else {
        if (input).starts_with(&("#####".to_string()) as &str) {
            level = 5_i64;
        } else {
            if (input).starts_with(&("####".to_string()) as &str) {
                level = 4_i64;
            } else {
                if (input).starts_with(&("###".to_string()) as &str) {
                    level = 3_i64;
                } else {
                    if (input).starts_with(&("##".to_string()) as &str) {
                        level = 2_i64;
                    } else {
                        level = 1_i64;
                    }
                }
            }
        }
    }
    if !(((level > 0_i64) && (level <= 6_i64))) {
        return Err(ParseError { message: "invalid heading level".to_string(), position: pos });
    }
    let text = (input).trim().to_string();
    Ok(MarkdownNode::Heading { level: level, text: text })
}

pub fn parse_bold(input: String, pos: i64) -> ParseResult {
    if !((input).starts_with(&("**".to_string()) as &str)) {
        return Err(ParseError { message: "not bold: does not start with **".to_string(), position: pos });
    }
    if !((input).ends_with(&("**".to_string()) as &str)) {
        return Err(ParseError { message: "unclosed bold: missing closing **".to_string(), position: pos });
    }
    let text = (input).trim().to_string();
    Ok(MarkdownNode::Bold { text: text })
}

pub fn parse_italic(input: String, pos: i64) -> ParseResult {
    if !((input).starts_with(&("*".to_string()) as &str)) {
        return Err(ParseError { message: "not italic: does not start with *".to_string(), position: pos });
    }
    if (input).starts_with(&("**".to_string()) as &str) {
        return Err(ParseError { message: "not italic: this is bold markup".to_string(), position: pos });
    }
    if !((input).ends_with(&("*".to_string()) as &str)) {
        return Err(ParseError { message: "unclosed italic: missing closing *".to_string(), position: pos });
    }
    let text = (input).trim().to_string();
    Ok(MarkdownNode::Italic { text: text })
}

pub fn parse_code_block(input: String, pos: i64) -> ParseResult {
    if !((input).starts_with(&("```".to_string()) as &str)) {
        return Err(ParseError { message: "not a code block: does not start with ```".to_string(), position: pos });
    }
    if !((input).ends_with(&("```".to_string()) as &str)) {
        return Err(ParseError { message: "unclosed code block: missing closing ```".to_string(), position: pos });
    }
    let lang = (input).trim().to_string();
    let content = (input).trim().to_string();
    Ok(MarkdownNode::CodeBlock { lang: lang, content: content })
}

pub fn parse_link(input: String, pos: i64) -> ParseResult {
    if !((input).starts_with(&("[".to_string()) as &str)) {
        return Err(ParseError { message: "not a link: does not start with [".to_string(), position: pos });
    }
    if !((input).contains(&("](".to_string()) as &str)) {
        return Err(ParseError { message: "malformed link: missing ]( separator".to_string(), position: pos });
    }
    if !((input).ends_with(&(")".to_string()) as &str)) {
        return Err(ParseError { message: "malformed link: missing closing )".to_string(), position: pos });
    }
    let text = (input).trim().to_string();
    let url = (input).trim().to_string();
    Ok(MarkdownNode::Link { text: text, url: url })
}

pub fn parse_list_item(input: String, pos: i64) -> ParseResult {
    let is_dash = (input).starts_with(&("- ".to_string()) as &str);
    let is_star = (input).starts_with(&("* ".to_string()) as &str);
    if !((is_dash || is_star)) {
        return Err(ParseError { message: "not a list item".to_string(), position: pos });
    }
    let text = (input).trim().to_string();
    Ok(MarkdownNode::ListItem { text: text })
}

pub fn parse_line(input: String) -> ParseResult {
    if !((((input).chars().count() as i64) > 0_i64)) {
        return Ok(MarkdownNode::Text { content: "".to_string() });
    }
    let heading = parse_heading(input, 0_i64);
    match heading {
        Ok(node) => {
            return Ok(node);
        }
        Err(_) => {
        }
    }
    let code = parse_code_block(input, 0_i64);
    match code {
        Ok(node) => {
            return Ok(node);
        }
        Err(_) => {
        }
    }
    let bold = parse_bold(input, 0_i64);
    match bold {
        Ok(node) => {
            return Ok(node);
        }
        Err(_) => {
        }
    }
    let italic = parse_italic(input, 0_i64);
    match italic {
        Ok(node) => {
            return Ok(node);
        }
        Err(_) => {
        }
    }
    let link = parse_link(input, 0_i64);
    match link {
        Ok(node) => {
            return Ok(node);
        }
        Err(_) => {
        }
    }
    let list_item = parse_list_item(input, 0_i64);
    match list_item {
        Ok(node) => {
            return Ok(node);
        }
        Err(_) => {
        }
    }
    Ok(MarkdownNode::Text { content: input })
}

fn render_node(node: MarkdownNode) -> String {
    match node {
        MarkdownNode::Heading { level, text } => {
            let prefix = if (level == 1_i64) { "#".to_string() } else { if (level == 2_i64) { "##".to_string() } else { if (level == 3_i64) { "###".to_string() } else { if (level == 4_i64) { "####".to_string() } else { if (level == 5_i64) { "#####".to_string() } else { "######".to_string() } } } } };
            format!("{} {}", prefix, text)
        }
        MarkdownNode::Paragraph { text } => text,
        MarkdownNode::CodeBlock { lang, content } => format!("```{}
{}
```", lang, content),
        MarkdownNode::Bold { text } => format!("**{}**", text),
        MarkdownNode::Italic { text } => format!("*{}*", text),
        MarkdownNode::Link { text, url } => format!("[{}]({})", text, url),
        MarkdownNode::ListItem { text } => format!("- {}", text),
        MarkdownNode::Text { content } => content,
    }
}

fn render_document(nodes: Vec<MarkdownNode>) -> String {
    let rendered = nodes.map(|node: _| render_node(node));
    join_strings(rendered, "\n".to_string())
}

fn parse_lines(lines: Vec<String>) -> Vec<MarkdownNode> {
    let mut nodes: Vec<MarkdownNode> = vec![];
    for line in lines {
        let result = parse_line(line);
        let node = match result {
            Ok(n) => n,
            Err(e) => MarkdownNode::Text { content: format!("PARSE ERROR: {}", e.message) },
        };
        nodes = (nodes + vec![node]);
    }
    nodes
}

fn parse_document(lines: Vec<String>) -> Vec<MarkdownNode> {
    parse_lines(lines)
}

fn main() {
    let lines: Vec<String> = vec!["# Markdown Parser".to_string(), "".to_string(), "A recursive descent parser written in Bock.".to_string(), "".to_string(), "## Features".to_string(), "".to_string(), "- Heading detection".to_string(), "- Bold and italic recognition".to_string(), "- Code block parsing".to_string(), "- Link extraction".to_string(), "- List item support".to_string(), "".to_string(), "### Implementation Notes".to_string(), "".to_string(), "This parser processes one line at a time.".to_string(), "Each line is tested against parsers in priority order.".to_string(), "".to_string(), "**Important**: The parser uses guard clauses for validation.".to_string(), "".to_string(), "*Note*: Falling back to Text for unrecognized lines.".to_string(), "".to_string(), "[Bock Language](https://bock-lang.dev)".to_string(), "".to_string(), "```bock".to_string(), "fn hello() -> String { \"world\" }".to_string(), "```".to_string()];
    println!("{}", "=== Parsing Markdown Document ===".to_string());
    println!("{}", "".to_string());
    let nodes = parse_document(lines);
    let mut headings = 0_i64;
    let mut text_nodes = 0_i64;
    let mut list_items = 0_i64;
    let mut bold_count = 0_i64;
    let mut italic_count = 0_i64;
    let mut link_count = 0_i64;
    let mut code_count = 0_i64;
    for node in nodes {
        match node {
            MarkdownNode::Heading { level, text } => {
                headings = (headings + 1_i64);
                println!("{}", format!("Found H{}: {}", level, text))
            }
            MarkdownNode::ListItem { text } => {
                list_items = (list_items + 1_i64);
                println!("{}", format!("Found list item: {}", text))
            }
            MarkdownNode::Bold { text } => {
                bold_count = (bold_count + 1_i64);
                println!("{}", format!("Found bold: {}", text))
            }
            MarkdownNode::Italic { text } => {
                italic_count = (italic_count + 1_i64);
                println!("{}", format!("Found italic: {}", text))
            }
            MarkdownNode::Link { text, url } => {
                link_count = (link_count + 1_i64);
                println!("{}", format!("Found link: {} -> {}", text, url))
            }
            MarkdownNode::CodeBlock { lang, content } => {
                code_count = (code_count + 1_i64);
                println!("{}", format!("Found code block ({})", lang))
            }
            MarkdownNode::Text { content } => {
                text_nodes = (text_nodes + 1_i64);
            }
            MarkdownNode::Paragraph { text } => {
                text_nodes = (text_nodes + 1_i64);
            }
        }
    }
    println!("{}", "".to_string());
    println!("{}", "=== Document Statistics ===".to_string());
    println!("{}", format!("Headings:    {}", headings));
    println!("{}", format!("List items:  {}", list_items));
    println!("{}", format!("Bold:        {}", bold_count));
    println!("{}", format!("Italic:      {}", italic_count));
    println!("{}", format!("Links:       {}", link_count));
    println!("{}", format!("Code blocks: {}", code_count));
    println!("{}", format!("Text nodes:  {}", text_nodes));
    println!("{}", "".to_string());
    println!("{}", "=== Rendered Output ===".to_string());
    let output = render_document(nodes.clone());
    println!("{}", output)
}
