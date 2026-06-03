package main

import (
	"fmt"
	"strings"
	"unicode/utf8"
)

type MarkdownNode interface {
	isMarkdownNode()
}

type MarkdownNodeHeading struct {
	Level	int64
	Text	string
}

func (MarkdownNodeHeading) isMarkdownNode() {}

type MarkdownNodeParagraph struct {
	Text	string
}

func (MarkdownNodeParagraph) isMarkdownNode() {}

type MarkdownNodeCodeBlock struct {
	Lang	string
	Content	string
}

func (MarkdownNodeCodeBlock) isMarkdownNode() {}

type MarkdownNodeBold struct {
	Text	string
}

func (MarkdownNodeBold) isMarkdownNode() {}

type MarkdownNodeItalic struct {
	Text	string
}

func (MarkdownNodeItalic) isMarkdownNode() {}

type MarkdownNodeLink struct {
	Text	string
	Url	string
}

func (MarkdownNodeLink) isMarkdownNode() {}

type MarkdownNodeListItem struct {
	Text	string
}

func (MarkdownNodeListItem) isMarkdownNode() {}

type MarkdownNodeText struct {
	Content	string
}

func (MarkdownNodeText) isMarkdownNode() {}

type ParseError struct {
	Message	string
	Position	int64
}

type ParseResult = interface{}

func joinStrings(items []string, sep string) string {
	result := ""
	first := true
	for _, item := range items {
		if first {
			result = item
			first = false
		} else {
			result = ((result + sep) + item)
		}
	}
	return result
}

func ParseHeading(input string, pos int64) ParseResult {
	if !(strings.HasPrefix(input, "#")) {
		return __bockErr(ParseError{Message: "not a heading: does not start with #", Position: pos})
	}
	level := 0
	if strings.HasPrefix(input, "######") {
		level = 6
	} else {
		if strings.HasPrefix(input, "#####") {
			level = 5
		} else {
			if strings.HasPrefix(input, "####") {
				level = 4
			} else {
				if strings.HasPrefix(input, "###") {
					level = 3
				} else {
					if strings.HasPrefix(input, "##") {
						level = 2
					} else {
						level = 1
					}
				}
			}
		}
	}
	if !(((level > 0) && (level <= 6))) {
		return __bockErr(ParseError{Message: "invalid heading level", Position: pos})
	}
	text := strings.TrimSpace(input)
	return __bockOk(MarkdownNodeHeading{Level: level, Text: text})
}

func ParseBold(input string, pos int64) ParseResult {
	if !(strings.HasPrefix(input, "**")) {
		return __bockErr(ParseError{Message: "not bold: does not start with **", Position: pos})
	}
	if !(strings.HasSuffix(input, "**")) {
		return __bockErr(ParseError{Message: "unclosed bold: missing closing **", Position: pos})
	}
	text := strings.TrimSpace(input)
	return __bockOk(MarkdownNodeBold{Text: text})
}

func ParseItalic(input string, pos int64) ParseResult {
	if !(strings.HasPrefix(input, "*")) {
		return __bockErr(ParseError{Message: "not italic: does not start with *", Position: pos})
	}
	if strings.HasPrefix(input, "**") {
		return __bockErr(ParseError{Message: "not italic: this is bold markup", Position: pos})
	}
	if !(strings.HasSuffix(input, "*")) {
		return __bockErr(ParseError{Message: "unclosed italic: missing closing *", Position: pos})
	}
	text := strings.TrimSpace(input)
	return __bockOk(MarkdownNodeItalic{Text: text})
}

func ParseCodeBlock(input string, pos int64) ParseResult {
	if !(strings.HasPrefix(input, "```")) {
		return __bockErr(ParseError{Message: "not a code block: does not start with ```", Position: pos})
	}
	if !(strings.HasSuffix(input, "```")) {
		return __bockErr(ParseError{Message: "unclosed code block: missing closing ```", Position: pos})
	}
	lang := strings.TrimSpace(input)
	content := strings.TrimSpace(input)
	return __bockOk(MarkdownNodeCodeBlock{Lang: lang, Content: content})
}

func ParseLink(input string, pos int64) ParseResult {
	if !(strings.HasPrefix(input, "[")) {
		return __bockErr(ParseError{Message: "not a link: does not start with [", Position: pos})
	}
	if !(strings.Contains(input, "](")) {
		return __bockErr(ParseError{Message: "malformed link: missing ]( separator", Position: pos})
	}
	if !(strings.HasSuffix(input, ")")) {
		return __bockErr(ParseError{Message: "malformed link: missing closing )", Position: pos})
	}
	text := strings.TrimSpace(input)
	url := strings.TrimSpace(input)
	return __bockOk(MarkdownNodeLink{Text: text, Url: url})
}

func ParseListItem(input string, pos int64) ParseResult {
	isDash := strings.HasPrefix(input, "- ")
	isStar := strings.HasPrefix(input, "* ")
	if !((isDash || isStar)) {
		return __bockErr(ParseError{Message: "not a list item", Position: pos})
	}
	text := strings.TrimSpace(input)
	return __bockOk(MarkdownNodeListItem{Text: text})
}

func ParseLine(input string) ParseResult {
	if !((int64(utf8.RuneCountInString(input)) > 0)) {
		return __bockOk(MarkdownNodeText{Content: ""})
	}
	heading := ParseHeading(input, 0)
	__res := heading
	if __res.tag == "Ok" { node := __res.v; _ = node; 
		return __bockOk(node)
	} else { 
		// empty
	}
	code := ParseCodeBlock(input, 0)
	__res := code
	if __res.tag == "Ok" { node := __res.v; _ = node; 
		return __bockOk(node)
	} else { 
		// empty
	}
	bold := ParseBold(input, 0)
	__res := bold
	if __res.tag == "Ok" { node := __res.v; _ = node; 
		return __bockOk(node)
	} else { 
		// empty
	}
	italic := ParseItalic(input, 0)
	__res := italic
	if __res.tag == "Ok" { node := __res.v; _ = node; 
		return __bockOk(node)
	} else { 
		// empty
	}
	link := ParseLink(input, 0)
	__res := link
	if __res.tag == "Ok" { node := __res.v; _ = node; 
		return __bockOk(node)
	} else { 
		// empty
	}
	listItem := ParseListItem(input, 0)
	__res := listItem
	if __res.tag == "Ok" { node := __res.v; _ = node; 
		return __bockOk(node)
	} else { 
		// empty
	}
	return __bockOk(MarkdownNodeText{Content: input})
}

func renderNode(node MarkdownNode) string {
	return func() string { switch node.(type) { case MarkdownNodeHeading: return func() interface{} { return fmt.Sprintf("%v %v", prefix, text) }(); case MarkdownNodeParagraph: return text; case MarkdownNodeCodeBlock: return fmt.Sprintf("```%v\n%v\n```", lang, content); case MarkdownNodeBold: return fmt.Sprintf("**%v**", text); case MarkdownNodeItalic: return fmt.Sprintf("*%v*", text); case MarkdownNodeLink: return fmt.Sprintf("[%v](%v)", text, url); case MarkdownNodeListItem: return fmt.Sprintf("- %v", text); case MarkdownNodeText: return content; }; panic("unreachable") }()
}

func renderDocument(nodes []MarkdownNode) string {
	rendered := nodes.map(func(node interface{}) string { return renderNode(node) })
	return joinStrings(rendered, "\n")
}

func parseLines(lines []string) []MarkdownNode {
	var nodes []MarkdownNode = []MarkdownNode{}
	for _, line := range lines {
		result := ParseLine(line)
		node := func() interface{} { __res := result; if __res.tag == "Ok" { n := __res.v; _ = n; return n } else { e := __res.v; _ = e; return MarkdownNodeText{Content: fmt.Sprintf("PARSE ERROR: %v", e.Message)} }; return nil }()
		nodes = (nodes + []interface{}{node})
	}
	return nodes
}

func parseDocument(lines []string) []MarkdownNode {
	return parseLines(lines)
}

func main() {
	var lines []string = []string{"# Markdown Parser", "", "A recursive descent parser written in Bock.", "", "## Features", "", "- Heading detection", "- Bold and italic recognition", "- Code block parsing", "- Link extraction", "- List item support", "", "### Implementation Notes", "", "This parser processes one line at a time.", "Each line is tested against parsers in priority order.", "", "**Important**: The parser uses guard clauses for validation.", "", "*Note*: Falling back to Text for unrecognized lines.", "", "[Bock Language](https://bock-lang.dev)", "", "```bock", "fn hello() -> String { \"world\" }", "```"}
	fmt.Println("=== Parsing Markdown Document ===")
	fmt.Println("")
	nodes := parseDocument(lines)
	headings := 0
	textNodes := 0
	listItems := 0
	boldCount := 0
	italicCount := 0
	linkCount := 0
	codeCount := 0
	for _, node := range nodes {
		switch __v := node.(type) {
			case MarkdownNodeHeading:
				level := __v.Level; _ = level
				text := __v.Text; _ = text
				headings = (headings + 1)
				fmt.Println(fmt.Sprintf("Found H%v: %v", level, text))
				case MarkdownNodeListItem:
					text := __v.Text; _ = text
					listItems = (listItems + 1)
					fmt.Println(fmt.Sprintf("Found list item: %v", text))
					case MarkdownNodeBold:
						text := __v.Text; _ = text
						boldCount = (boldCount + 1)
						fmt.Println(fmt.Sprintf("Found bold: %v", text))
						case MarkdownNodeItalic:
							text := __v.Text; _ = text
							italicCount = (italicCount + 1)
							fmt.Println(fmt.Sprintf("Found italic: %v", text))
							case MarkdownNodeLink:
								text := __v.Text; _ = text
								url := __v.Url; _ = url
								linkCount = (linkCount + 1)
								fmt.Println(fmt.Sprintf("Found link: %v -> %v", text, url))
								case MarkdownNodeCodeBlock:
									lang := __v.Lang; _ = lang
									content := __v.Content; _ = content
									codeCount = (codeCount + 1)
									fmt.Println(fmt.Sprintf("Found code block (%v)", lang))
									case MarkdownNodeText:
										content := __v.Content; _ = content
										textNodes = (textNodes + 1)
										case MarkdownNodeParagraph:
											text := __v.Text; _ = text
											textNodes = (textNodes + 1)
											default:
												panic(fmt.Sprintf("unreachable match arm: %v", __v))
										}
									}
									fmt.Println("")
									fmt.Println("=== Document Statistics ===")
									fmt.Println(fmt.Sprintf("Headings:    %v", headings))
									fmt.Println(fmt.Sprintf("List items:  %v", listItems))
									fmt.Println(fmt.Sprintf("Bold:        %v", boldCount))
									fmt.Println(fmt.Sprintf("Italic:      %v", italicCount))
									fmt.Println(fmt.Sprintf("Links:       %v", linkCount))
									fmt.Println(fmt.Sprintf("Code blocks: %v", codeCount))
									fmt.Println(fmt.Sprintf("Text nodes:  %v", textNodes))
									fmt.Println("")
									fmt.Println("=== Rendered Output ===")
									output := renderDocument(nodes)
									fmt.Println(output)
								}
