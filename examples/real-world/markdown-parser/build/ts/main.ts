import type { BockResult } from "./_bock_runtime.js";
export type MarkdownNode = MarkdownNode_Heading | MarkdownNode_Paragraph | MarkdownNode_CodeBlock | MarkdownNode_Bold | MarkdownNode_Italic | MarkdownNode_Link | MarkdownNode_ListItem | MarkdownNode_Text;

interface MarkdownNode_Heading {
  readonly _tag: "Heading";
  readonly level: number;
  readonly text: string;
}
function MarkdownNode_Heading(level: number, text: string): MarkdownNode_Heading {
  return { _tag: "Heading" as const, level, text };
}
interface MarkdownNode_Paragraph {
  readonly _tag: "Paragraph";
  readonly text: string;
}
function MarkdownNode_Paragraph(text: string): MarkdownNode_Paragraph {
  return { _tag: "Paragraph" as const, text };
}
interface MarkdownNode_CodeBlock {
  readonly _tag: "CodeBlock";
  readonly lang: string;
  readonly content: string;
}
function MarkdownNode_CodeBlock(lang: string, content: string): MarkdownNode_CodeBlock {
  return { _tag: "CodeBlock" as const, lang, content };
}
interface MarkdownNode_Bold {
  readonly _tag: "Bold";
  readonly text: string;
}
function MarkdownNode_Bold(text: string): MarkdownNode_Bold {
  return { _tag: "Bold" as const, text };
}
interface MarkdownNode_Italic {
  readonly _tag: "Italic";
  readonly text: string;
}
function MarkdownNode_Italic(text: string): MarkdownNode_Italic {
  return { _tag: "Italic" as const, text };
}
interface MarkdownNode_Link {
  readonly _tag: "Link";
  readonly text: string;
  readonly url: string;
}
function MarkdownNode_Link(text: string, url: string): MarkdownNode_Link {
  return { _tag: "Link" as const, text, url };
}
interface MarkdownNode_ListItem {
  readonly _tag: "ListItem";
  readonly text: string;
}
function MarkdownNode_ListItem(text: string): MarkdownNode_ListItem {
  return { _tag: "ListItem" as const, text };
}
interface MarkdownNode_Text {
  readonly _tag: "Text";
  readonly content: string;
}
function MarkdownNode_Text(content: string): MarkdownNode_Text {
  return { _tag: "Text" as const, content };
}

export class ParseError {
  message: string;
  position: number;
  constructor({ message, position }: { message: string; position: number }) {
    this.message = message;
    this.position = position;
  }
}

export type ParseResult = BockResult<MarkdownNode, ParseError>;

function joinStrings(items: Array<string>, sep: string): string {
  let result = "";
  let first = true;
  for (const item of items) {
    if (first) {
      result = item;
      first = false;
    } else {
      result = ((result + sep) + item);
    }
  }
  return result;
}

export function parseHeading(input: string, pos: number): ParseResult {
  if (!((input).startsWith("#"))) {
    return { _tag: "Err" as const, _0: new ParseError({ message: "not a heading: does not start with #", position: pos }) };
  }
  let level = 0;
  if ((input).startsWith("######")) {
    level = 6;
  } else {
    if ((input).startsWith("#####")) {
      level = 5;
    } else {
      if ((input).startsWith("####")) {
        level = 4;
      } else {
        if ((input).startsWith("###")) {
          level = 3;
        } else {
          if ((input).startsWith("##")) {
            level = 2;
          } else {
            level = 1;
          }
        }
      }
    }
  }
  if (!(((level > 0) && (level <= 6)))) {
    return { _tag: "Err" as const, _0: new ParseError({ message: "invalid heading level", position: pos }) };
  }
  const text = (input).trim();
  return { _tag: "Ok" as const, _0: MarkdownNode_Heading(level, text) };
}

export function parseBold(input: string, pos: number): ParseResult {
  if (!((input).startsWith("**"))) {
    return { _tag: "Err" as const, _0: new ParseError({ message: "not bold: does not start with **", position: pos }) };
  }
  if (!((input).endsWith("**"))) {
    return { _tag: "Err" as const, _0: new ParseError({ message: "unclosed bold: missing closing **", position: pos }) };
  }
  const text = (input).trim();
  return { _tag: "Ok" as const, _0: MarkdownNode_Bold(text) };
}

export function parseItalic(input: string, pos: number): ParseResult {
  if (!((input).startsWith("*"))) {
    return { _tag: "Err" as const, _0: new ParseError({ message: "not italic: does not start with *", position: pos }) };
  }
  if ((input).startsWith("**")) {
    return { _tag: "Err" as const, _0: new ParseError({ message: "not italic: this is bold markup", position: pos }) };
  }
  if (!((input).endsWith("*"))) {
    return { _tag: "Err" as const, _0: new ParseError({ message: "unclosed italic: missing closing *", position: pos }) };
  }
  const text = (input).trim();
  return { _tag: "Ok" as const, _0: MarkdownNode_Italic(text) };
}

export function parseCodeBlock(input: string, pos: number): ParseResult {
  if (!((input).startsWith("```"))) {
    return { _tag: "Err" as const, _0: new ParseError({ message: "not a code block: does not start with ```", position: pos }) };
  }
  if (!((input).endsWith("```"))) {
    return { _tag: "Err" as const, _0: new ParseError({ message: "unclosed code block: missing closing ```", position: pos }) };
  }
  const lang = (input).trim();
  const content = (input).trim();
  return { _tag: "Ok" as const, _0: MarkdownNode_CodeBlock(lang, content) };
}

export function parseLink(input: string, pos: number): ParseResult {
  if (!((input).startsWith("["))) {
    return { _tag: "Err" as const, _0: new ParseError({ message: "not a link: does not start with [", position: pos }) };
  }
  if (!((input).includes("]("))) {
    return { _tag: "Err" as const, _0: new ParseError({ message: "malformed link: missing ]( separator", position: pos }) };
  }
  if (!((input).endsWith(")"))) {
    return { _tag: "Err" as const, _0: new ParseError({ message: "malformed link: missing closing )", position: pos }) };
  }
  const text = (input).trim();
  const url = (input).trim();
  return { _tag: "Ok" as const, _0: MarkdownNode_Link(text, url) };
}

export function parseListItem(input: string, pos: number): ParseResult {
  const isDash = (input).startsWith("- ");
  const isStar = (input).startsWith("* ");
  if (!((isDash || isStar))) {
    return { _tag: "Err" as const, _0: new ParseError({ message: "not a list item", position: pos }) };
  }
  const text = (input).trim();
  return { _tag: "Ok" as const, _0: MarkdownNode_ListItem(text) };
}

export function parseLine(input: string): ParseResult {
  if (!(([...(input)].length > 0))) {
    return { _tag: "Ok" as const, _0: MarkdownNode_Text("") };
  }
  const heading = parseHeading(input, 0);
  switch (heading._tag) {
    case "Ok": {
      const node = heading._0;
      return { _tag: "Ok" as const, _0: node };
      break;
    }
    case "Err": {
      const _ = heading._0;
      break;
    }
  }
  const code = parseCodeBlock(input, 0);
  switch (code._tag) {
    case "Ok": {
      const node = code._0;
      return { _tag: "Ok" as const, _0: node };
      break;
    }
    case "Err": {
      const _ = code._0;
      break;
    }
  }
  const bold = parseBold(input, 0);
  switch (bold._tag) {
    case "Ok": {
      const node = bold._0;
      return { _tag: "Ok" as const, _0: node };
      break;
    }
    case "Err": {
      const _ = bold._0;
      break;
    }
  }
  const italic = parseItalic(input, 0);
  switch (italic._tag) {
    case "Ok": {
      const node = italic._0;
      return { _tag: "Ok" as const, _0: node };
      break;
    }
    case "Err": {
      const _ = italic._0;
      break;
    }
  }
  const link = parseLink(input, 0);
  switch (link._tag) {
    case "Ok": {
      const node = link._0;
      return { _tag: "Ok" as const, _0: node };
      break;
    }
    case "Err": {
      const _ = link._0;
      break;
    }
  }
  const listItem = parseListItem(input, 0);
  switch (listItem._tag) {
    case "Ok": {
      const node = listItem._0;
      return { _tag: "Ok" as const, _0: node };
      break;
    }
    case "Err": {
      const _ = listItem._0;
      break;
    }
  }
  return { _tag: "Ok" as const, _0: MarkdownNode_Text(input) };
}

function renderNode(node: MarkdownNode): string {
  return (() => {
    switch (node._tag) {
      case "Heading": {
        const level = node.level;
        const text = node.text;
        const prefix = ((level === 1) ? "#" : ((level === 2) ? "##" : ((level === 3) ? "###" : ((level === 4) ? "####" : ((level === 5) ? "#####" : "######")))));
        return `${prefix} ${text}`;
        break;
      }
      case "Paragraph": {
        const text = node.text;
        return text;
        break;
      }
      case "CodeBlock": {
        const lang = node.lang;
        const content = node.content;
        return `\`\`\`${lang}
${content}
\`\`\``;
        break;
      }
      case "Bold": {
        const text = node.text;
        return `**${text}**`;
        break;
      }
      case "Italic": {
        const text = node.text;
        return `*${text}*`;
        break;
      }
      case "Link": {
        const text = node.text;
        const url = node.url;
        return `[${text}](${url})`;
        break;
      }
      case "ListItem": {
        const text = node.text;
        return `- ${text}`;
        break;
      }
      case "Text": {
        const content = node.content;
        return content;
        break;
      }
    }
  })();
}

function renderDocument(nodes: Array<MarkdownNode>): string {
  const rendered = nodes.map(nodes, (node) => renderNode(node));
  return joinStrings(rendered, "\n");
}

function parseLines(lines: Array<string>): Array<MarkdownNode> {
  let nodes: Array<MarkdownNode> = [];
  for (const line of lines) {
    const result = parseLine(line);
    const node = (() => {
      switch (result._tag) {
        case "Ok": {
          const n = result._0;
          return n;
          break;
        }
        case "Err": {
          const e = result._0;
          return MarkdownNode_Text(`PARSE ERROR: ${e.message}`);
          break;
        }
      }
    })();
    nodes = (nodes + [node]);
  }
  return nodes;
}

function parseDocument(lines: Array<string>): Array<MarkdownNode> {
  return parseLines(lines);
}

function main() {
  const lines: Array<string> = ["# Markdown Parser", "", "A recursive descent parser written in Bock.", "", "## Features", "", "- Heading detection", "- Bold and italic recognition", "- Code block parsing", "- Link extraction", "- List item support", "", "### Implementation Notes", "", "This parser processes one line at a time.", "Each line is tested against parsers in priority order.", "", "**Important**: The parser uses guard clauses for validation.", "", "*Note*: Falling back to Text for unrecognized lines.", "", "[Bock Language](https://bock-lang.dev)", "", "```bock", "fn hello() -> String { \"world\" }", "```"];
  console.log("=== Parsing Markdown Document ===");
  console.log("");
  const nodes = parseDocument(lines);
  let headings = 0;
  let textNodes = 0;
  let listItems = 0;
  let boldCount = 0;
  let italicCount = 0;
  let linkCount = 0;
  let codeCount = 0;
  for (const node of nodes) {
    switch (node._tag) {
      case "Heading": {
        const level = node.level;
        const text = node.text;
        headings = (headings + 1);
        return console.log(`Found H${level}: ${text}`);
        break;
      }
      case "ListItem": {
        const text = node.text;
        listItems = (listItems + 1);
        return console.log(`Found list item: ${text}`);
        break;
      }
      case "Bold": {
        const text = node.text;
        boldCount = (boldCount + 1);
        return console.log(`Found bold: ${text}`);
        break;
      }
      case "Italic": {
        const text = node.text;
        italicCount = (italicCount + 1);
        return console.log(`Found italic: ${text}`);
        break;
      }
      case "Link": {
        const text = node.text;
        const url = node.url;
        linkCount = (linkCount + 1);
        return console.log(`Found link: ${text} -> ${url}`);
        break;
      }
      case "CodeBlock": {
        const lang = node.lang;
        const content = node.content;
        codeCount = (codeCount + 1);
        return console.log(`Found code block (${lang})`);
        break;
      }
      case "Text": {
        const content = node.content;
        textNodes = (textNodes + 1);
        break;
      }
      case "Paragraph": {
        const text = node.text;
        textNodes = (textNodes + 1);
        break;
      }
    }
  }
  console.log("");
  console.log("=== Document Statistics ===");
  console.log(`Headings:    ${headings}`);
  console.log(`List items:  ${listItems}`);
  console.log(`Bold:        ${boldCount}`);
  console.log(`Italic:      ${italicCount}`);
  console.log(`Links:       ${linkCount}`);
  console.log(`Code blocks: ${codeCount}`);
  console.log(`Text nodes:  ${textNodes}`);
  console.log("");
  console.log("=== Rendered Output ===");
  const output = renderDocument(nodes);
  return console.log(output);
}
export { MarkdownNode_Bold, MarkdownNode_CodeBlock, MarkdownNode_Heading, MarkdownNode_Italic, MarkdownNode_Link, MarkdownNode_ListItem, MarkdownNode_Paragraph, MarkdownNode_Text };
main();
//# sourceMappingURL=main.ts.map
