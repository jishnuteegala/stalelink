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
	"github.com/yuin/goldmark/text"
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
	if len(os.Args) > 5 {
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
	documents := len(records)
	if filter == "" {
		entries, _ := os.ReadDir(os.Args[2])
		documents = len(entries)
	} else {
		seen := map[string]bool{}
		for _, r := range records {
			seen[r.Doc] = true
		}
		documents = len(seen)
	}
	json.NewEncoder(os.Stdout).Encode(receipt{documents, len(records), fmt.Sprintf("%x", sum), records, median(times), formats})
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
	cursor := 0
	for {
		tt := z.Next()
		if tt == html.ErrorToken {
			break
		}
		if tt != html.StartTagToken && tt != html.SelfClosingTagToken {
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
				at := bytes.Index(data[cursor:], []byte(a.Val))
				if at >= 0 {
					at += cursor
					cursor = at + len(a.Val)
					addText(doc, data, a.Val, at, &out)
				}
			}
		}
	}
	return out
}

// Goldmark supplies Markdown grammar and semantic destinations. Source ranges are then
// located inside the parsed node range only, preserving raw byte spans for parity checks.
func markdownLinks(doc string, data []byte) []record {
	source := text.NewReader(data)
	root := goldmark.DefaultParser().Parse(source)
	var out []record
	ast.Walk(root, func(node ast.Node, entering bool) (ast.WalkStatus, error) {
		if !entering {
			return ast.WalkContinue, nil
		}
		var raw string
		switch n := node.(type) {
		case *ast.Link:
			raw = html.UnescapeString(string(n.Destination))
		case *ast.Image:
			raw = html.UnescapeString(string(n.Destination))
		case *ast.AutoLink:
			raw = string(n.URL(data))
		case *ast.LinkReferenceDefinition:
			raw = string(n.Destination)
		}
		if raw == "" || !valid(raw) {
			return ast.WalkContinue, nil
		}
		segments := node.Text(data)
		at := bytes.Index(data, []byte(raw))
		if len(segments) > 0 {
			if candidate := bytes.Index(data, []byte(raw)); candidate >= 0 {
				at = candidate
			}
		}
		if at >= 0 {
			addText(doc, data, raw, at, &out)
		}
		return ast.WalkContinue, nil
	})
	return out
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
