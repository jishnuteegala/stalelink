// THROWAWAY benchmark-only port of stalelink's extraction core. It is not
// production code and is deliberately kept outside the Rust workspace.
package main

import (
 "archive/zip"
 "fmt"
 "io"
 "os"
 "path/filepath"
 "regexp"
 "strings"
)

var url = regexp.MustCompile(`https?://[^\s<>"')]+`)
var href = regexp.MustCompile(`(?i)href\s*=\s*["'](https?://[^"']+)["']`)
var target = regexp.MustCompile(`(?i)Target="(https?://[^"]+)"`)

func main() {
 if len(os.Args) != 3 || os.Args[1] != "extract" { panic("usage: stalelink-go extract <directory>") }
 entries, err := os.ReadDir(os.Args[2]); if err != nil { panic(err) }
 total := 0
 for _, entry := range entries { if entry.IsDir() { continue }; count, err := extract(filepath.Join(os.Args[2], entry.Name()), strings.ToLower(filepath.Ext(entry.Name()))); if err != nil { panic(err) }; total += count }
 fmt.Println(total)
}

func extract(path, extension string) (int, error) {
 bytes, err := os.ReadFile(path); if err != nil { return 0, err }; text := string(bytes)
 switch extension {
 case ".html": return len(href.FindAllStringSubmatch(text, -1)), nil
 case ".md", ".txt", ".pdf": return len(url.FindAllString(text, -1)), nil
 case ".docx", ".xlsx", ".pptx":
  reader, err := zip.OpenReader(path); if err != nil { return 0, err }; defer reader.Close(); count := 0
  for _, file := range reader.File { if !strings.HasSuffix(file.Name, ".rels") { continue }; stream, err := file.Open(); if err != nil { return 0, err }; data, err := io.ReadAll(stream); stream.Close(); if err != nil { return 0, err }; count += len(target.FindAllStringSubmatch(string(data), -1)) }
  return count, nil
 default: return 0, nil
 }
}
