function MarkdownNode_Heading(level, text) {
  return { _tag: "Heading", level, text };
}
function MarkdownNode_Paragraph(text) {
  return { _tag: "Paragraph", text };
}
function MarkdownNode_CodeBlock(lang, content) {
  return { _tag: "CodeBlock", lang, content };
}
function MarkdownNode_Bold(text) {
  return { _tag: "Bold", text };
}
function MarkdownNode_Italic(text) {
  return { _tag: "Italic", text };
}
function MarkdownNode_Link(text, url) {
  return { _tag: "Link", text, url };
}
function MarkdownNode_ListItem(text) {
  return { _tag: "ListItem", text };
}
function MarkdownNode_Text(content) {
  return { _tag: "Text", content };
}

class ParseError {
  constructor({ message, position }) {
    this.message = message;
    this.position = position;
  }
}

// type ParseResult = ...

function joinStrings(items, sep) {
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

export function parseHeading(input, pos) {
  if (!((input).startsWith("#"))) {
    return { _tag: "Err", _0: new ParseError({ message: "not a heading: does not start with #", position: pos }) };
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
    return { _tag: "Err", _0: new ParseError({ message: "invalid heading level", position: pos }) };
  }
  const text = (input).trim();
  return { _tag: "Ok", _0: MarkdownNode_Heading(level, text) };
}

export function parseBold(input, pos) {
  if (!((input).startsWith("**"))) {
    return { _tag: "Err", _0: new ParseError({ message: "not bold: does not start with **", position: pos }) };
  }
  if (!((input).endsWith("**"))) {
    return { _tag: "Err", _0: new ParseError({ message: "unclosed bold: missing closing **", position: pos }) };
  }
  const text = (input).trim();
  return { _tag: "Ok", _0: MarkdownNode_Bold(text) };
}

export function parseItalic(input, pos) {
  if (!((input).startsWith("*"))) {
    return { _tag: "Err", _0: new ParseError({ message: "not italic: does not start with *", position: pos }) };
  }
  if ((input).startsWith("**")) {
    return { _tag: "Err", _0: new ParseError({ message: "not italic: this is bold markup", position: pos }) };
  }
  if (!((input).endsWith("*"))) {
    return { _tag: "Err", _0: new ParseError({ message: "unclosed italic: missing closing *", position: pos }) };
  }
  const text = (input).trim();
  return { _tag: "Ok", _0: MarkdownNode_Italic(text) };
}

export function parseCodeBlock(input, pos) {
  if (!((input).startsWith("```"))) {
    return { _tag: "Err", _0: new ParseError({ message: "not a code block: does not start with ```", position: pos }) };
  }
  if (!((input).endsWith("```"))) {
    return { _tag: "Err", _0: new ParseError({ message: "unclosed code block: missing closing ```", position: pos }) };
  }
  const lang = (input).trim();
  const content = (input).trim();
  return { _tag: "Ok", _0: MarkdownNode_CodeBlock(lang, content) };
}

export function parseLink(input, pos) {
  if (!((input).startsWith("["))) {
    return { _tag: "Err", _0: new ParseError({ message: "not a link: does not start with [", position: pos }) };
  }
  if (!((input).includes("]("))) {
    return { _tag: "Err", _0: new ParseError({ message: "malformed link: missing ]( separator", position: pos }) };
  }
  if (!((input).endsWith(")"))) {
    return { _tag: "Err", _0: new ParseError({ message: "malformed link: missing closing )", position: pos }) };
  }
  const text = (input).trim();
  const url = (input).trim();
  return { _tag: "Ok", _0: MarkdownNode_Link(text, url) };
}

export function parseListItem(input, pos) {
  const isDash = (input).startsWith("- ");
  const isStar = (input).startsWith("* ");
  if (!((isDash || isStar))) {
    return { _tag: "Err", _0: new ParseError({ message: "not a list item", position: pos }) };
  }
  const text = (input).trim();
  return { _tag: "Ok", _0: MarkdownNode_ListItem(text) };
}

export function parseLine(input) {
  if (!(([...(input)].length > 0))) {
    return { _tag: "Ok", _0: MarkdownNode_Text("") };
  }
  const heading = parseHeading(input, 0);
  switch (heading._tag) {
    case "Ok": {
      const node = heading._0;
      return { _tag: "Ok", _0: node };
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
      return { _tag: "Ok", _0: node };
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
      return { _tag: "Ok", _0: node };
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
      return { _tag: "Ok", _0: node };
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
      return { _tag: "Ok", _0: node };
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
      return { _tag: "Ok", _0: node };
      break;
    }
    case "Err": {
      const _ = listItem._0;
      break;
    }
  }
  return { _tag: "Ok", _0: MarkdownNode_Text(input) };
}

function renderNode(node) {
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

function renderDocument(nodes) {
  const rendered = nodes.map(nodes, (node) => renderNode(node));
  return joinStrings(rendered, "\n");
}

function parseLines(lines) {
  let nodes = [];
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

function parseDocument(lines) {
  return parseLines(lines);
}

function main() {
  const lines = ["# Markdown Parser", "", "A recursive descent parser written in Bock.", "", "## Features", "", "- Heading detection", "- Bold and italic recognition", "- Code block parsing", "- Link extraction", "- List item support", "", "### Implementation Notes", "", "This parser processes one line at a time.", "Each line is tested against parsers in priority order.", "", "**Important**: The parser uses guard clauses for validation.", "", "*Note*: Falling back to Text for unrecognized lines.", "", "[Bock Language](https://bock-lang.dev)", "", "```bock", "fn hello() -> String { \"world\" }", "```"];
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
export { MarkdownNode_Bold, MarkdownNode_CodeBlock, MarkdownNode_Heading, MarkdownNode_Italic, MarkdownNode_Link, MarkdownNode_ListItem, MarkdownNode_Paragraph, MarkdownNode_Text, ParseError };
main();
//# sourceMappingURL=main.js.map
