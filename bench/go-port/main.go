// THROWAWAY benchmark-only port of stalelink's extraction core. It is not
// production code and is deliberately kept outside the Rust workspace.
package main

import (
	"archive/zip"
	"bytes"
	"compress/zlib"
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

	"golang.org/x/net/html"
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
type xmlRelationship struct {
	ID     string `xml:"Id,attr"`
	Type   string `xml:"Type,attr"`
	Target string `xml:"Target,attr"`
	Mode   string `xml:"TargetMode,attr"`
}
type xmlRelationships struct {
	Items []xmlRelationship `xml:"Relationship"`
}

func main() {
	if len(os.Args) < 3 {
		panic("usage: stalelink-go <extract|throughput> <directory> [warmup passes]")
	}
	warmup, passes := 0, 1
	if os.Args[1] == "throughput" {
		fmt.Sscanf(os.Args[3], "%d", &warmup)
		fmt.Sscanf(os.Args[4], "%d", &passes)
	}
	for range warmup {
		all(os.Args[2])
	}
	times := make([]float64, 0, passes)
	var records []record
	for range passes {
		start := time.Now()
		records = all(os.Args[2])
		times = append(times, time.Since(start).Seconds())
	}
	sort.Float64s(times)
	formats := map[string]int{}
	for _, r := range records {
		formats[extensionFormat(filepath.Ext(r.Doc))]++
	}
	encoded, _ := json.Marshal(records)
	sum := sha256.Sum256(encoded)
	entries, _ := os.ReadDir(os.Args[2])
	json.NewEncoder(os.Stdout).Encode(receipt{len(entries), len(records), fmt.Sprintf("%x", sum), records, times[len(times)/2], formats})
}

func all(dir string) []record {
	entries, err := os.ReadDir(dir)
	if err != nil {
		panic(err)
	}
	sort.Slice(entries, func(i, j int) bool { return entries[i].Name() < entries[j].Name() })
	var out []record
	for _, e := range entries {
		if !e.IsDir() {
			path := filepath.Join(dir, e.Name())
			data, err := os.ReadFile(path)
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
	case ".docx", ".xlsx", ".pptx":
		return ooxmlLinks(doc, data)
	case ".pdf":
		return pdfLinks(doc, data)
	}
	return nil
}
func bareLinks(doc string, data []byte) []record {
	var out []record
	cursor := 0
	for _, word := range strings.Fields(string(data)) {
		at := bytes.Index(data[cursor:], []byte(word))
		if at >= 0 {
			at += cursor
			cursor = at + len(word)
			raw := strings.TrimRight(word, ".,;:!?)")
			addText(doc, data, raw, at, &out)
		}
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
		if token.Data == "a" || token.Data == "link" {
			attr = "href"
		}
		if token.Data == "img" || token.Data == "script" {
			attr = "src"
		}
		for _, a := range token.Attr {
			if a.Key == attr && valid(a.Val) {
				needle := []byte(a.Val)
				at := bytes.Index(data[cursor:], needle)
				if at >= 0 {
					at += cursor
					cursor = at + len(needle)
					addText(doc, data, a.Val, at, &out)
				}
			}
		}
	}
	return out
}
func markdownLinks(doc string, data []byte) []record {
	s := string(data)
	var out []record // Inline, autolink, and reference destinations are emitted with raw URL spans.
	for i := 0; i < len(s); i++ {
		if s[i] == '<' {
			if end := strings.IndexByte(s[i+1:], '>'); end >= 0 {
				raw := s[i+1 : i+1+end]
				addText(doc, data, raw, i+1, &out)
				i += end + 1
			}
		}
		if s[i] == '(' {
			if end := strings.IndexByte(s[i+1:], ')'); end >= 0 {
				raw := s[i+1 : i+1+end]
				if strings.Contains(s[max(0, i-2):i], "]") {
					addText(doc, data, raw, i+1, &out)
				}
				i += end + 1
			}
		}
		if s[i] == '[' {
			if close := strings.Index(s[i:], "]:"); close >= 0 {
				start := i + close + 2
				for start < len(s) && s[start] == ' ' {
					start++
				}
				end := start
				for end < len(s) && s[end] != ' ' && s[end] != '\n' {
					end++
				}
				addText(doc, data, s[start:end], start, &out)
				i = end
			}
		}
	}
	return out
}
func max(a, b int) int {
	if a > b {
		return a
	}
	return b
}

func ooxmlLinks(doc string, data []byte) []record {
	zr, err := zip.NewReader(bytes.NewReader(data), int64(len(data)))
	if err != nil {
		panic(err)
	}
	files := map[string][]byte{}
	for _, f := range zr.File {
		r, _ := f.Open()
		b, _ := io.ReadAll(r)
		r.Close()
		files[f.Name] = b
	}
	rels := map[string]map[string]string{}
	for name, b := range files {
		if strings.HasSuffix(name, ".rels") {
			var rs xmlRelationships
			if xml.Unmarshal(b, &rs) == nil {
				m := map[string]string{}
				for _, r := range rs.Items {
					if r.Mode == "External" && strings.HasSuffix(r.Type, "/hyperlink") && valid(r.Target) {
						m[r.ID] = r.Target
					}
				}
				rels[name] = m
			}
		}
	}
	var out []record
	for name, b := range files {
		if !strings.HasSuffix(name, ".xml") || strings.Contains(name, "_rels/") {
			continue
		}
		rel := path.Dir(name) + "/_rels/" + path.Base(name) + ".rels"
		for id, target := range rels[rel] {
			marker := []byte("r:id=\"" + id + "\"")
			if at := bytes.Index(b, marker); at >= 0 {
				loc := map[string]any{"type": "docx", "paragraph": bytes.Count(b[:at], []byte("<w:p"))}
				if filepath.Ext(doc) == ".xlsx" {
					cell := ""
					before := string(b[:at])
					if i := strings.LastIndex(before, "<hyperlink ref=\""); i >= 0 {
						rest := before[i+16:]
						cell = strings.Split(rest, "\"")[0]
					}
					loc = map[string]any{"type": "xlsx", "sheet": "Generated", "cell": cell}
				}
				if filepath.Ext(doc) == ".pptx" {
					loc = map[string]any{"type": "pptx", "slide": 1}
				}
				out = append(out, record{doc, target, loc, nil})
			}
		}
	}
	return out
}

func pdfLinks(doc string, data []byte) []record { // Object-aware scan: streams are decoded according to their dictionary before URL extraction.
	s := string(data)
	var out []record
	for _, piece := range strings.Split(s, "endobj") {
		if !strings.Contains(piece, "obj") {
			continue
		}
		page := 1
		if strings.Contains(piece, "/Subtype /Link") && strings.Contains(piece, "/S /URI") {
			if uri := strings.Index(piece, "/URI ("); uri >= 0 {
				raw := strings.Split(piece[uri+6:], ")")[0]
				if valid(raw) {
					out = append(out, record{doc, raw, map[string]any{"type": "pdf", "page": page, "annotation": 0}, nil})
				}
			}
		}
		if strings.Contains(piece, "stream") {
			stream := strings.Split(strings.Split(piece, "stream")[1], "endstream")[0]
			if strings.Contains(piece, "/FlateDecode") {
				r, err := zlib.NewReader(strings.NewReader(strings.TrimLeft(stream, "\r\n")))
				if err != nil {
					continue
				}
				b, err := io.ReadAll(r)
				r.Close()
				if err != nil {
					continue
				}
				stream = string(b)
			}
			for _, word := range strings.Fields(stream) {
				raw := strings.Trim(word, "()")
				if valid(raw) {
					out = append(out, record{doc, raw, map[string]any{"type": "pdf", "page": page, "annotation": nil}, nil})
				}
			}
		}
	}
	return out
}
