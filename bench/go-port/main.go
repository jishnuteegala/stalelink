// THROWAWAY benchmark-only port of stalelink's extraction core. It is not
// production code and is deliberately kept outside the Rust workspace.
package main

import (
	"archive/zip"
	"bytes"
	"crypto/sha256"
	"encoding/json"
	"encoding/xml"
	"fmt"
	"io"
	"net/url"
	"os"
	"path"
	"path/filepath"
	"sort"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/pdfcpu/pdfcpu/pkg/api"
	"github.com/pdfcpu/pdfcpu/pkg/pdfcpu/model"
	"github.com/pdfcpu/pdfcpu/pkg/pdfcpu/types"
	"github.com/yuin/goldmark"
	"github.com/yuin/goldmark/ast"
	"github.com/yuin/goldmark/parser"
	"github.com/yuin/goldmark/text"
	"github.com/yuin/goldmark/util"
	"golang.org/x/net/html"
)

const (
	wordNS = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
	presNS = "http://schemas.openxmlformats.org/presentationml/2006/main"
	drawNS = "http://schemas.openxmlformats.org/drawingml/2006/main"
	relNS  = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
)

type record struct {
	Doc      string  `json:"doc"`
	URL      string  `json:"url"`
	Location any     `json:"location"`
	Span     *[2]int `json:"span"`
}
type receipt struct {
	Documents     int            `json:"documents"`
	Links         int            `json:"links"`
	Digest        string         `json:"digest"`
	Records       []record       `json:"records"`
	MedianSeconds float64        `json:"median_seconds"`
	Formats       map[string]int `json:"formats"`
}
type relationship struct {
	ID     string `xml:"Id,attr"`
	Type   string `xml:"Type,attr"`
	Target string `xml:"Target,attr"`
	Mode   string `xml:"TargetMode,attr"`
}
type relationships struct {
	Items []relationship `xml:"Relationship"`
}

func main() {
	if len(os.Args) < 3 || (os.Args[1] == "throughput" && len(os.Args) < 5) {
		panic("usage: stalelink-go <extract|throughput> <directory> [warmup-passes timed-passes format]")
	}
	warmup, passes := 0, 1
	if os.Args[1] == "throughput" {
		fmt.Sscanf(os.Args[3], "%d", &warmup)
		fmt.Sscanf(os.Args[4], "%d", &passes)
	}
	filter := ""
	if os.Args[1] == "extract" && len(os.Args) > 3 {
		filter = os.Args[3]
	} else if len(os.Args) > 5 {
		filter = os.Args[5]
	}
	for range warmup {
		all(os.Args[2], filter)
	}
	times := make([]float64, 0, passes)
	var records []record
	for range passes {
		start := time.Now()
		records = all(os.Args[2], filter)
		times = append(times, time.Since(start).Seconds())
	}
	sort.Float64s(times)
	formats := map[string]int{}
	for _, r := range records {
		formats[extensionFormat(filepath.Ext(r.Doc))]++
	}
	encoded, _ := json.Marshal(records)
	sum := sha256.Sum256(encoded)
	documents := documentCount(os.Args[2], filter)
	json.NewEncoder(os.Stdout).Encode(receipt{documents, len(records), fmt.Sprintf("%x", sum), records, median(times), formats})
}
func documentCount(dir, filter string) int {
	entries, err := os.ReadDir(dir)
	if err != nil {
		panic(err)
	}
	count := 0
	for _, entry := range entries {
		if !entry.IsDir() && (filter == "" || filepath.Ext(entry.Name()) == "."+filter) {
			count++
		}
	}
	return count
}

func median(values []float64) float64 {
	n := len(values)
	if n%2 == 1 {
		return values[n/2]
	}
	return (values[n/2-1] + values[n/2]) / 2
}
func all(dir, filter string) []record {
	entries, err := os.ReadDir(dir)
	if err != nil {
		panic(err)
	}
	sort.Slice(entries, func(i, j int) bool { return entries[i].Name() < entries[j].Name() })
	var out []record
	for _, e := range entries {
		if filter != "" && filepath.Ext(e.Name()) != "."+filter {
			continue
		}
		if !e.IsDir() {
			data, err := os.ReadFile(filepath.Join(dir, e.Name()))
			if err != nil {
				panic(err)
			}
			out = append(out, extract(e.Name(), data)...)
		}
	}
	sort.Slice(out, func(i, j int) bool {
		a, _ := json.Marshal(out[i])
		b, _ := json.Marshal(out[j])
		return bytes.Compare(a, b) < 0
	})
	return out
}
func valid(raw string) bool {
	u, err := url.Parse(raw)
	return err == nil && (u.Scheme == "http" || u.Scheme == "https")
}
func extensionFormat(ext string) string {
	return map[string]string{".md": "markdown", ".html": "html", ".txt": "text", ".pdf": "pdf", ".docx": "docx", ".xlsx": "xlsx", ".pptx": "pptx"}[ext]
}
func textLocation(data []byte, at int) map[string]any {
	line, col := 1, 1
	for len(data[:at]) > 0 {
		r, size := utf8.DecodeRune(data[:at])
		data = data[size:]
		at -= size
		if r == '\n' {
			line, col = line+1, 1
		} else {
			col++
		}
	}
	return map[string]any{"type": "text", "line": line, "column": col}
}
func addText(doc string, data []byte, raw string, start int, out *[]record) {
	if valid(raw) {
		span := [2]int{start, start + len(raw)}
		*out = append(*out, record{doc, raw, textLocation(data, start), &span})
	}
}
func addTextSpan(doc string, data []byte, value string, start, end int, out *[]record) {
	if valid(value) {
		span := [2]int{start, end}
		*out = append(*out, record{doc, value, textLocation(data, start), &span})
	}
}
func extract(doc string, data []byte) []record {
	switch filepath.Ext(doc) {
	case ".html":
		return htmlLinks(doc, data)
	case ".md":
		return markdownLinks(doc, data)
	case ".txt":
		return bareLinks(doc, data)
	case ".docx":
		return docxLinks(doc, data)
	case ".xlsx":
		return xlsxLinks(doc, data)
	case ".pptx":
		return pptxLinks(doc, data)
	case ".pdf":
		return pdfLinks(doc, data)
	}
	return nil
}

// This recognizes URLs in prose rather than treating whitespace-delimited words as URLs.
func bareLinks(doc string, data []byte) []record {
	var out []record
	for i := 0; i < len(data); i++ {
		if !bytes.HasPrefix(data[i:], []byte("http://")) && !bytes.HasPrefix(data[i:], []byte("https://")) {
			continue
		}
		end := i
		for end < len(data) && !strings.ContainsRune(" \t\r\n<>\"'", rune(data[end])) {
			end++
		}
		for end > i && strings.ContainsRune(".,;:!?)", rune(data[end-1])) {
			end--
		}
		addText(doc, data, string(data[i:end]), i, &out)
		i = end
	}
	return out
}
func htmlLinks(doc string, data []byte) []record {
	z := html.NewTokenizer(bytes.NewReader(data))
	var out []record
	offset := 0
	for {
		tt := z.Next()
		raw := append([]byte(nil), z.Raw()...)
		if tt == html.ErrorToken {
			break
		}
		if tt != html.StartTagToken && tt != html.SelfClosingTagToken {
			offset += len(raw)
			continue
		}
		token := z.Token()
		attr := ""
		switch strings.ToLower(token.Data) {
		case "a", "link":
			attr = "href"
		case "img", "script":
			attr = "src"
		}
		for _, a := range token.Attr {
			if a.Key == attr && valid(a.Val) {
				if start, end, ok := htmlAttributeSpan(raw, attr); ok {
					addTextSpan(doc, data, a.Val, offset+start, offset+end, &out)
				}
			}
		}
		offset += len(raw)
	}
	return out
}

// Raw returns the exact bytes for this token, so attribute scanning is confined
// to one tag and cannot confuse repeated or entity-decoded values elsewhere.
func htmlAttributeSpan(raw []byte, want string) (int, int, bool) {
	for i := 0; i < len(raw); {
		for i < len(raw) && (raw[i] == '<' || raw[i] == '>' || raw[i] == '/' || raw[i] == ' ' || raw[i] == '\t' || raw[i] == '\r' || raw[i] == '\n') {
			i++
		}
		nameStart := i
		for i < len(raw) && !strings.ContainsRune("= />\t\r\n", rune(raw[i])) {
			i++
		}
		name := string(raw[nameStart:i])
		for i < len(raw) && (raw[i] == ' ' || raw[i] == '\t' || raw[i] == '\r' || raw[i] == '\n') {
			i++
		}
		if i >= len(raw) || raw[i] != '=' {
			continue
		}
		i++
		for i < len(raw) && (raw[i] == ' ' || raw[i] == '\t' || raw[i] == '\r' || raw[i] == '\n') {
			i++
		}
		start := i
		if i < len(raw) && (raw[i] == '\'' || raw[i] == '"') {
			quote := raw[i]
			start++
			i++
			for i < len(raw) && raw[i] != quote {
				i++
			}
			end := i
			if i < len(raw) {
				i++
			}
			if strings.EqualFold(name, want) {
				return start, end, true
			}
			continue
		}
		for i < len(raw) && !strings.ContainsRune(" >\t\r\n", rune(raw[i])) {
			i++
		}
		if strings.EqualFold(name, want) {
			return start, i, true
		}
	}
	return 0, 0, false
}

// Goldmark supplies grammar, decoded destinations, node positions, and block segments.
// Raw destination spans are reconstructed only inside those parser-owned ranges.
func markdownLinks(doc string, data []byte) []record {
	source := text.NewReader(data)
	definitions := map[string]markdownDefinition{}
	markdown := goldmark.New(goldmark.WithParserOptions(parser.WithParagraphTransformers(
		util.Prioritized(referenceDefinitionCollector{definitions}, 99),
	)))
	root := markdown.Parser().Parse(source)
	var out []record
	autolinkCursor := map[ast.Node]int{}
	ast.Walk(root, func(node ast.Node, entering bool) (ast.WalkStatus, error) {
		if !entering {
			return ast.WalkContinue, nil
		}
		var raw string
		switch n := node.(type) {
		case *ast.Link:
			raw = markdownValue(string(n.Destination))
		case *ast.Image:
			raw = markdownValue(string(n.Destination))
		case *ast.AutoLink:
			raw = string(n.URL(data))
			parent := node.Parent()
			start, end := parent.Pos(), segmentEnd(parent)
			if next, ok := autolinkCursor[parent]; ok {
				start = next
			}
			if start >= 0 && end >= start && end <= len(data) {
				if offset := bytes.Index(data[start:end], []byte(raw)); offset >= 0 {
					at := start + offset
					autolinkCursor[parent] = at + len(raw)
					addText(doc, data, raw, at, &out)
				}
			}
			return ast.WalkContinue, nil
		case *ast.LinkReferenceDefinition:
			return ast.WalkContinue, nil
		}
		if raw == "" || !valid(raw) {
			return ast.WalkContinue, nil
		}
		if link, ok := node.(*ast.Link); ok && link.Reference != nil {
			label := string(link.Reference.Value)
			if link.Reference.Type != ast.ReferenceLinkFull {
				label = string(link.Text(data))
			}
			if definition, ok := definitions[strings.ToLower(label)]; ok {
				addTextSpan(doc, data, raw, definition.span[0], definition.span[1], &out)
			}
			return ast.WalkContinue, nil
		}
		if start, end, ok := markdownDestination(data, node.Pos(), segmentEnd(node)); ok {
			addTextSpan(doc, data, raw, start, end, &out)
		}
		return ast.WalkContinue, nil
	})
	sort.Slice(out, func(i, j int) bool { return out[i].Span[0] < out[j].Span[0] })
	for i := 1; i < len(out); i++ {
		if out[i].Span != nil && out[i-1].Span != nil && *out[i].Span == *out[i-1].Span {
			out = append(out[:i], out[i+1:]...)
			i--
		}
	}
	return out
}

type markdownDefinition struct{ span [2]int }

// Runs immediately after Goldmark creates reference-definition nodes, before
// parsing inlines consumes them from the finished AST.
type referenceDefinitionCollector struct {
	definitions map[string]markdownDefinition
}

func (c referenceDefinitionCollector) Transform(node *ast.Paragraph, reader text.Reader, _ parser.Context) {
	parent := node.Parent()
	for sibling := parent.FirstChild(); sibling != nil; sibling = sibling.NextSibling() {
		definition, ok := sibling.(*ast.LinkReferenceDefinition)
		if !ok {
			continue
		}
		if lines := definition.Lines(); lines.Len() > 0 {
			start := definition.Pos()
			end := lines.At(lines.Len() - 1).Stop
			end = referenceDefinitionEnd(reader.Source(), definition, start, end)
			if valueStart, valueEnd, ok := markdownDestination(reader.Source(), start, end); ok {
				c.definitions[strings.ToLower(string(definition.Label))] = markdownDefinition{[2]int{valueStart, valueEnd}}
			}
		}
	}
}

// Goldmark retains a parsed destination even when a reference definition's
// segment list ends at its label line. A matching destination makes the line
// range complete; otherwise include exactly the next physical line Goldmark
// consumed as the destination/title continuation.
func referenceDefinitionEnd(data []byte, definition *ast.LinkReferenceDefinition, start, end int) int {
	if valueStart, valueEnd, ok := markdownDestination(data, start, end); ok && markdownValue(string(data[valueStart:valueEnd])) == string(definition.Destination) {
		return end
	}
	if end < 0 || end >= len(data) {
		return end
	}
	if newline := bytes.IndexByte(data[end:], '\n'); newline >= 0 {
		return end + newline + 1
	}
	return len(data)
}

func markdownValue(raw string) string {
	var value strings.Builder
	escaped := false
	for _, r := range html.UnescapeString(raw) {
		if escaped {
			value.WriteRune(r)
			escaped = false
		} else if r == '\\' {
			escaped = true
		} else {
			value.WriteRune(r)
		}
	}
	if escaped {
		value.WriteByte('\\')
	}
	return value.String()
}

func segmentEnd(node ast.Node) int {
	for n := node; n != nil; n = n.Parent() {
		if n.Type() != ast.TypeBlock {
			continue
		}
		if lines := n.Lines(); lines != nil && lines.Len() > 0 {
			return lines.At(lines.Len() - 1).Stop
		}
	}
	return -1
}
func markdownDestination(data []byte, start, end int) (int, int, bool) {
	if start < 0 || end < start || end > len(data) {
		return 0, 0, false
	}
	close := -1
	depth := 0
	escaped := false
	for i := start; i < end; i++ {
		if escaped {
			escaped = false
			continue
		}
		if data[i] == '\\' {
			escaped = true
			continue
		}
		if data[i] == '[' {
			depth++
		}
		if data[i] == ']' {
			depth--
			if depth == 0 {
				close = i
				break
			}
		}
	}
	if close < 0 {
		return 0, 0, false
	}
	i := close + 1
	if i >= end || data[i] != '(' { // reference definition: [label]: destination
		for i < end && markdownDefinitionSpace(data[i]) {
			i++
		}
		if i >= end || data[i] != ':' {
			return 0, 0, false
		}
		i++
		for i < end && markdownDefinitionSpace(data[i]) {
			i++
		}
	} else {
		i++
	}
	if i < end && data[i] == '<' {
		i++
		value := i
		for i < end && data[i] != '>' {
			i++
		}
		return value, i, i < end
	}
	value, depth, escaped := i, 0, false
	for i < end {
		if escaped {
			escaped = false
			i++
			continue
		}
		if data[i] == '\\' {
			escaped = true
			i++
			continue
		}
		if data[i] == '(' {
			depth++
		}
		if (data[i] == ')' && depth == 0) || (data[i] == ' ' || data[i] == '\t' || data[i] == '\n') {
			break
		}
		if data[i] == ')' {
			depth--
		}
		i++
	}
	return value, i, i > value
}

func zipFiles(data []byte) map[string][]byte {
	zr, err := zip.NewReader(bytes.NewReader(data), int64(len(data)))
	if err != nil {
		panic(err)
	}
	files := map[string][]byte{}
	for _, f := range zr.File {
		r, err := f.Open()
		if err != nil {
			panic(err)
		}
		b, err := io.ReadAll(r)
		r.Close()
		if err != nil {
			panic(err)
		}
		files[f.Name] = b
	}
	return files
}
func relationMap(data []byte, external bool) map[string]string {
	var rs relationships
	if xml.Unmarshal(data, &rs) != nil {
		return nil
	}
	out := map[string]string{}
	for _, r := range rs.Items {
		if (!external || r.Mode == "External") && (!external || valid(r.Target)) {
			out[r.ID] = r.Target
		}
	}
	return out
}

func markdownDefinitionSpace(b byte) bool {
	return b == ' ' || b == '\t' || b == '\r' || b == '\n'
}
func relationshipPart(part string) string {
	return path.Dir(part) + "/_rels/" + path.Base(part) + ".rels"
}
func attr(start xml.StartElement, space, local string) string {
	for _, a := range start.Attr {
		if a.Name.Space == space && a.Name.Local == local {
			return a.Value
		}
	}
	return ""
}
func localAttr(start xml.StartElement, local string) string {
	for _, a := range start.Attr {
		if a.Name.Local == local {
			return a.Value
		}
	}
	return ""
}
func fieldURL(value string) string {
	i := strings.Index(value, "HYPERLINK")
	if i < 0 {
		return ""
	}
	rest := value[i+len("HYPERLINK"):]
	i = strings.Index(rest, "http")
	if i < 0 {
		return ""
	}
	rest = rest[i:]
	if i = strings.IndexFunc(rest, func(r rune) bool { return r == '"' || r == ')' || r == ' ' || r == '\t' || r == '\n' }); i >= 0 {
		rest = rest[:i]
	}
	if valid(rest) {
		return rest
	}
	return ""
}

func docxLinks(doc string, data []byte) []record {
	files, rels := zipFiles(data), relationMap(zipFiles(data)["word/_rels/document.xml.rels"], true)
	dec := xml.NewDecoder(bytes.NewReader(files["word/document.xml"]))
	paragraph := 0
	var out []record
	var field strings.Builder
	inField := false
	for {
		tok, err := dec.Token()
		if err == io.EOF {
			break
		}
		if err != nil {
			panic(err)
		}
		switch t := tok.(type) {
		case xml.StartElement:
			if t.Name.Space != wordNS {
				continue
			}
			switch t.Name.Local {
			case "p":
				paragraph++
			case "hyperlink":
				if u := rels[attr(t, relNS, "id")]; u != "" {
					out = append(out, record{doc, u, map[string]any{"type": "docx", "paragraph": paragraph}, nil})
				}
			case "fldSimple":
				if u := fieldURL(localAttr(t, "instr")); u != "" {
					out = append(out, record{doc, u, map[string]any{"type": "docx", "paragraph": paragraph}, nil})
				}
			case "fldChar":
				if localAttr(t, "fldCharType") == "begin" {
					inField = true
					field.Reset()
				}
				if localAttr(t, "fldCharType") == "end" {
					if u := fieldURL(field.String()); u != "" {
						out = append(out, record{doc, u, map[string]any{"type": "docx", "paragraph": paragraph}, nil})
					}
					inField = false
				}
			}
		case xml.CharData:
			if inField {
				field.Write([]byte(t))
			}
		}
	}
	return out
}
func xlsxLinks(doc string, data []byte) []record {
	files := zipFiles(data)
	wbRels := relationMap(files["xl/_rels/workbook.xml.rels"], false)
	dec := xml.NewDecoder(bytes.NewReader(files["xl/workbook.xml"]))
	type sheet struct{ name, part string }
	var sheets []sheet
	for {
		tok, err := dec.Token()
		if err == io.EOF {
			break
		}
		if err != nil {
			panic(err)
		}
		if t, ok := tok.(xml.StartElement); ok && t.Name.Local == "sheet" {
			if target := wbRels[attr(t, relNS, "id")]; target != "" {
				sheets = append(sheets, sheet{localAttr(t, "name"), resolveTarget("xl/workbook.xml", target)})
			}
		}
	}
	var out []record
	for _, s := range sheets {
		rels := relationMap(files[relationshipPart(s.part)], true)
		dec = xml.NewDecoder(bytes.NewReader(files[s.part]))
		cell, formula, inFormula := "", "", false
		for {
			tok, err := dec.Token()
			if err == io.EOF {
				break
			}
			if err != nil {
				panic(err)
			}
			switch t := tok.(type) {
			case xml.StartElement:
				switch t.Name.Local {
				case "c":
					cell = localAttr(t, "r")
				case "hyperlink":
					if u := rels[attr(t, relNS, "id")]; u != "" {
						out = append(out, record{doc, u, map[string]any{"type": "xlsx", "sheet": s.name, "cell": localAttr(t, "ref")}, nil})
					}
				case "f":
					inFormula = true
					formula = ""
				}
			case xml.CharData:
				if inFormula {
					formula += string(t)
				}
			case xml.EndElement:
				if t.Name.Local == "f" {
					if u := fieldURL(formula); u != "" {
						out = append(out, record{doc, u, map[string]any{"type": "xlsx", "sheet": s.name, "cell": cell}, nil})
					}
					inFormula = false
				}
			}
		}
	}
	return out
}
func pptxLinks(doc string, data []byte) []record {
	files := zipFiles(data)
	rels := relationMap(files["ppt/_rels/presentation.xml.rels"], false)
	dec := xml.NewDecoder(bytes.NewReader(files["ppt/presentation.xml"]))
	var slides []string
	for {
		tok, err := dec.Token()
		if err == io.EOF {
			break
		}
		if err != nil {
			panic(err)
		}
		if t, ok := tok.(xml.StartElement); ok && t.Name.Space == presNS && t.Name.Local == "sldId" {
			if target := rels[attr(t, relNS, "id")]; target != "" {
				slides = append(slides, resolveTarget("ppt/presentation.xml", target))
			}
		}
	}
	if len(slides) == 0 {
		for name := range files {
			if strings.HasPrefix(name, "ppt/slides/slide") && strings.HasSuffix(name, ".xml") {
				slides = append(slides, name)
			}
		}
		sort.Strings(slides)
	}
	var out []record
	for i, slide := range slides {
		rels = relationMap(files[relationshipPart(slide)], true)
		dec = xml.NewDecoder(bytes.NewReader(files[slide]))
		for {
			tok, err := dec.Token()
			if err == io.EOF {
				break
			}
			if err != nil {
				panic(err)
			}
			if t, ok := tok.(xml.StartElement); ok && t.Name.Space == drawNS && (t.Name.Local == "hlinkClick" || t.Name.Local == "hlinkHover") {
				if u := rels[attr(t, relNS, "id")]; u != "" {
					out = append(out, record{doc, u, map[string]any{"type": "pptx", "slide": i + 1}, nil})
				}
			}
		}
	}
	return out
}
func resolveTarget(source, target string) string {
	if strings.HasPrefix(target, "/") {
		return strings.TrimPrefix(target, "/")
	}
	return path.Clean(path.Join(path.Dir(source), target))
}

// pdfcpu resolves xref entries, indirect references, page inheritance, and decoded streams.
func pdfLinks(doc string, data []byte) []record {
	ctx, err := api.ReadAndValidate(bytes.NewReader(data), model.NewDefaultConfiguration())
	if err != nil {
		panic(err)
	}
	var out []record
	for page := 1; page <= ctx.PageCount; page++ {
		d, _, _, err := ctx.PageDict(page, false)
		if err != nil {
			continue
		}
		if annots, err := ctx.DereferenceArray(d["Annots"]); err == nil {
			for i, a := range annots {
				ad, err := ctx.DereferenceDict(a)
				if err != nil {
					continue
				}
				action, err := ctx.DereferenceDict(ad["A"])
				if err != nil {
					continue
				}
				if action.NameEntry("S") != nil && *action.NameEntry("S") == "URI" {
					if u, err := ctx.DereferenceText(action["URI"]); err == nil && valid(u) {
						out = append(out, record{doc, u, map[string]any{"type": "pdf", "page": page, "annotation": i}, nil})
					}
				}
			}
		}
		content, err := ctx.PageContent(d, page)
		if err == nil {
			for _, r := range bareLinks(doc, content) {
				r.Location = map[string]any{"type": "pdf", "page": page, "annotation": nil}
				r.Span = nil
				out = append(out, r)
			}
		}
	}
	return out
}

var _ types.Object
