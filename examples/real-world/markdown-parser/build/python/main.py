from __future__ import annotations
from _bock_runtime import *
from typing import Union
from dataclasses import dataclass

@dataclass
class MarkdownNode_Heading:
    level: int
    text: str
    _tag: str = "Heading"

@dataclass
class MarkdownNode_Paragraph:
    text: str
    _tag: str = "Paragraph"

@dataclass
class MarkdownNode_CodeBlock:
    lang: str
    content: str
    _tag: str = "CodeBlock"

@dataclass
class MarkdownNode_Bold:
    text: str
    _tag: str = "Bold"

@dataclass
class MarkdownNode_Italic:
    text: str
    _tag: str = "Italic"

@dataclass
class MarkdownNode_Link:
    text: str
    url: str
    _tag: str = "Link"

@dataclass
class MarkdownNode_ListItem:
    text: str
    _tag: str = "ListItem"

@dataclass
class MarkdownNode_Text:
    content: str
    _tag: str = "Text"
MarkdownNode = Union[MarkdownNode_Heading, MarkdownNode_Paragraph, MarkdownNode_CodeBlock, MarkdownNode_Bold, MarkdownNode_Italic, MarkdownNode_Link, MarkdownNode_ListItem, MarkdownNode_Text]

@dataclass
class ParseError:
    message: str
    position: int

# type ParseResult = ...

def join_strings(items: list[str], sep: str) -> str:
    result = ""
    first = True
    for item in items:
        if first:
            result = item
            first = False
        else:
            result = ((result + sep) + item)
    return result

def parse_heading(input: str, pos: int) -> ParseResult:
    if not ((input).startswith("#")):
        return _BockErr(ParseError(message="not a heading: does not start with #", position=pos))
    level = 0
    if (input).startswith("######"):
        level = 6
    else:
        if (input).startswith("#####"):
            level = 5
        else:
            if (input).startswith("####"):
                level = 4
            else:
                if (input).startswith("###"):
                    level = 3
                else:
                    if (input).startswith("##"):
                        level = 2
                    else:
                        level = 1
    if not (((level > 0) and (level <= 6))):
        return _BockErr(ParseError(message="invalid heading level", position=pos))
    text = (input).strip()
    return _BockOk(MarkdownNode_Heading(level=level, text=text))

def parse_bold(input: str, pos: int) -> ParseResult:
    if not ((input).startswith("**")):
        return _BockErr(ParseError(message="not bold: does not start with **", position=pos))
    if not ((input).endswith("**")):
        return _BockErr(ParseError(message="unclosed bold: missing closing **", position=pos))
    text = (input).strip()
    return _BockOk(MarkdownNode_Bold(text=text))

def parse_italic(input: str, pos: int) -> ParseResult:
    if not ((input).startswith("*")):
        return _BockErr(ParseError(message="not italic: does not start with *", position=pos))
    if (input).startswith("**"):
        return _BockErr(ParseError(message="not italic: this is bold markup", position=pos))
    if not ((input).endswith("*")):
        return _BockErr(ParseError(message="unclosed italic: missing closing *", position=pos))
    text = (input).strip()
    return _BockOk(MarkdownNode_Italic(text=text))

def parse_code_block(input: str, pos: int) -> ParseResult:
    if not ((input).startswith("```")):
        return _BockErr(ParseError(message="not a code block: does not start with ```", position=pos))
    if not ((input).endswith("```")):
        return _BockErr(ParseError(message="unclosed code block: missing closing ```", position=pos))
    lang = (input).strip()
    content = (input).strip()
    return _BockOk(MarkdownNode_CodeBlock(lang=lang, content=content))

def parse_link(input: str, pos: int) -> ParseResult:
    if not ((input).startswith("[")):
        return _BockErr(ParseError(message="not a link: does not start with [", position=pos))
    if not ((("](") in (input))):
        return _BockErr(ParseError(message="malformed link: missing ]( separator", position=pos))
    if not ((input).endswith(")")):
        return _BockErr(ParseError(message="malformed link: missing closing )", position=pos))
    text = (input).strip()
    url = (input).strip()
    return _BockOk(MarkdownNode_Link(text=text, url=url))

def parse_list_item(input: str, pos: int) -> ParseResult:
    is_dash = (input).startswith("- ")
    is_star = (input).startswith("* ")
    if not ((is_dash or is_star)):
        return _BockErr(ParseError(message="not a list item", position=pos))
    text = (input).strip()
    return _BockOk(MarkdownNode_ListItem(text=text))

def parse_line(input: str) -> ParseResult:
    if not ((len(input) > 0)):
        return _BockOk(MarkdownNode_Text(content=""))
    heading = parse_heading(input, 0)
    match heading:
        case _BockOk(node):
            return _BockOk(node)
        case _BockErr(_):
            pass
    code = parse_code_block(input, 0)
    match code:
        case _BockOk(node):
            return _BockOk(node)
        case _BockErr(_):
            pass
    bold = parse_bold(input, 0)
    match bold:
        case _BockOk(node):
            return _BockOk(node)
        case _BockErr(_):
            pass
    italic = parse_italic(input, 0)
    match italic:
        case _BockOk(node):
            return _BockOk(node)
        case _BockErr(_):
            pass
    link = parse_link(input, 0)
    match link:
        case _BockOk(node):
            return _BockOk(node)
        case _BockErr(_):
            pass
    list_item = parse_list_item(input, 0)
    match list_item:
        case _BockOk(node):
            return _BockOk(node)
        case _BockErr(_):
            pass
    return _BockOk(MarkdownNode_Text(content=input))

def render_node(node: MarkdownNode) -> str:
    return (lambda __v: (lambda: f"{prefix} {text}")() if True else (text if True else (f"""```{lang}
{content}
```""" if True else (f"**{text}**" if True else (f"*{text}*" if True else (f"[{text}]({url})" if True else (f"- {text}" if True else (content))))))))(node)

def render_document(nodes: list[MarkdownNode]) -> str:
    rendered = nodes.map(lambda node: render_node(node))
    return join_strings(rendered, "\n")

def parse_lines(lines: list[str]) -> list[MarkdownNode]:
    nodes: list[MarkdownNode] = []
    for line in lines:
        result = parse_line(line)
        node = (lambda __v: (lambda n: n)(__v._0) if isinstance(__v, _BockOk) else ((lambda e: MarkdownNode_Text(content=f"PARSE ERROR: {e.message}"))(__v._0)))(result)
        nodes = (nodes + [node])
    return nodes

def parse_document(lines: list[str]) -> list[MarkdownNode]:
    return parse_lines(lines)

def main():
    lines: list[str] = ["# Markdown Parser", "", "A recursive descent parser written in Bock.", "", "## Features", "", "- Heading detection", "- Bold and italic recognition", "- Code block parsing", "- Link extraction", "- List item support", "", "### Implementation Notes", "", "This parser processes one line at a time.", "Each line is tested against parsers in priority order.", "", "**Important**: The parser uses guard clauses for validation.", "", "*Note*: Falling back to Text for unrecognized lines.", "", "[Bock Language](https://bock-lang.dev)", "", "```bock", "fn hello() -> String { \"world\" }", "```"]
    print("=== Parsing Markdown Document ===")
    print("")
    nodes = parse_document(lines)
    headings = 0
    text_nodes = 0
    list_items = 0
    bold_count = 0
    italic_count = 0
    link_count = 0
    code_count = 0
    for node in nodes:
        match node:
            case MarkdownNode_Heading(level=level, text=text):
                headings = (headings + 1)
                return print(f"Found H{level}: {text}")
            case MarkdownNode_ListItem(text=text):
                list_items = (list_items + 1)
                return print(f"Found list item: {text}")
            case MarkdownNode_Bold(text=text):
                bold_count = (bold_count + 1)
                return print(f"Found bold: {text}")
            case MarkdownNode_Italic(text=text):
                italic_count = (italic_count + 1)
                return print(f"Found italic: {text}")
            case MarkdownNode_Link(text=text, url=url):
                link_count = (link_count + 1)
                return print(f"Found link: {text} -> {url}")
            case MarkdownNode_CodeBlock(lang=lang, content=content):
                code_count = (code_count + 1)
                return print(f"Found code block ({lang})")
            case MarkdownNode_Text(content=content):
                text_nodes = (text_nodes + 1)
            case MarkdownNode_Paragraph(text=text):
                text_nodes = (text_nodes + 1)
    print("")
    print("=== Document Statistics ===")
    print(f"Headings:    {headings}")
    print(f"List items:  {list_items}")
    print(f"Bold:        {bold_count}")
    print(f"Italic:      {italic_count}")
    print(f"Links:       {link_count}")
    print(f"Code blocks: {code_count}")
    print(f"Text nodes:  {text_nodes}")
    print("")
    print("=== Rendered Output ===")
    output = render_document(nodes)
    return print(output)
if __name__ == "__main__":
    main()
