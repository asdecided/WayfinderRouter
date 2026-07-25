import Foundation

/// Block-level markdown for assistant turns (UX-006).
///
/// Built on `AttributedString`'s inline markdown parser rather than a package
/// dependency. Block structure is parsed here; presentation lives in
/// `MarkdownBlockView`, which keeps this type testable without SwiftUI.
///
/// The parser is deliberately block-oriented because of streaming (UX-016).
/// Every closed block keeps a stable identity while text is appended, so a
/// growing reply never re-lays-out content the reader has already read.

// MARK: - Blocks

enum MarkdownColumnAlignment: Equatable, Sendable {
  case leading
  case center
  case trailing
}

struct MarkdownListItem: Equatable, Sendable {
  /// Nesting depth, zero-based.
  let depth: Int
  /// Rendered marker: a bullet, or the ordinal for ordered lists.
  let marker: String
  let text: AttributedString
}

struct MarkdownList: Equatable, Sendable {
  let isOrdered: Bool
  let items: [MarkdownListItem]
}

struct MarkdownTable: Equatable, Sendable {
  let header: [AttributedString]
  let rows: [[AttributedString]]
  let alignments: [MarkdownColumnAlignment]
}

enum MarkdownBlock: Equatable, Sendable {
  case paragraph(AttributedString)
  case heading(level: Int, text: AttributedString)
  case list(MarkdownList)
  /// `isClosed` is false while a fence is still open, which happens on every
  /// delta of a streaming code block. Rendering an open fence as code from the
  /// first line avoids a reflow when the closing fence finally arrives.
  case code(language: String?, code: String, isClosed: Bool)
  case quote([AttributedString])
  case rule
  case table(MarkdownTable)
}

/// A block plus the stable identity the transcript renders it under.
struct MarkdownBlockNode: Equatable, Identifiable, Sendable {
  let id: Int
  let block: MarkdownBlock
}

// MARK: - Document

struct MarkdownDocument: Equatable, Sendable {
  var nodes: [MarkdownBlockNode]

  /// The source prefix consumed by every block except the last one. While a
  /// reply streams, only the final block can still change, so this prefix is
  /// what an incremental re-parse is allowed to reuse.
  var closedSource: String

  static let empty = MarkdownDocument(nodes: [], closedSource: "")

  var isEmpty: Bool { nodes.isEmpty }

  static func parse(_ source: String) -> MarkdownDocument {
    var parser = MarkdownParser(source: source)
    return parser.parse(idOffset: 0)
  }
}

// MARK: - Parser

private struct MarkdownParser {
  private let lines: [Substring]
  private var index = 0
  /// Index of the first line of the block currently being emitted.
  private var blockStartLine = 0

  init(source: String) {
    lines = source.split(separator: "\n", omittingEmptySubsequences: false)
  }

  mutating func parse(idOffset: Int) -> MarkdownDocument {
    var nodes: [MarkdownBlockNode] = []
    var lastBlockStartLine = 0

    while index < lines.count {
      blockStartLine = index
      guard let block = nextBlock() else {
        continue
      }
      lastBlockStartLine = blockStartLine
      nodes.append(
        MarkdownBlockNode(id: idOffset + nodes.count, block: block)
      )
    }

    let closedSource: String
    if nodes.isEmpty {
      closedSource = ""
    } else {
      closedSource = lines[0..<lastBlockStartLine]
        .map(String.init)
        .joined(separator: "\n")
      // The separator that terminates the last closed line belongs to the
      // prefix too, otherwise the reused and re-parsed halves would fuse.
    }

    return MarkdownDocument(
      nodes: nodes,
      closedSource: lastBlockStartLine == 0 ? "" : closedSource + "\n"
    )
  }

  // MARK: Block dispatch

  private mutating func nextBlock() -> MarkdownBlock? {
    let line = lines[index]

    if line.trimmingCharacters(in: .whitespaces).isEmpty {
      index += 1
      return nil
    }
    if let fence = fenceInfo(line) {
      return codeBlock(fence: fence)
    }
    if let heading = headingBlock(line) {
      index += 1
      return heading
    }
    if isThematicBreak(line) {
      index += 1
      return .rule
    }
    if isQuote(line) {
      return quoteBlock()
    }
    if listMarker(line) != nil {
      return listBlock()
    }
    if let table = tableBlock() {
      return table
    }
    return paragraphBlock()
  }

  // MARK: Fenced code

  private struct Fence {
    let indent: Int
    let character: Character
    let length: Int
    let language: String?
  }

  private func fenceInfo(_ line: Substring) -> Fence? {
    let indent = leadingIndent(line)
    guard indent < 4 else { return nil }
    let content = line.drop(while: { $0 == " " || $0 == "\t" })
    guard let first = content.first, first == "`" || first == "~" else {
      return nil
    }
    let run = content.prefix(while: { $0 == first })
    guard run.count >= 3 else { return nil }
    let rest = content.dropFirst(run.count)
      .trimmingCharacters(in: .whitespaces)
    // An info string on a backtick fence may not itself contain a backtick.
    if first == "`", rest.contains(where: { $0 == "`" }) { return nil }
    return Fence(
      indent: indent,
      character: first,
      length: run.count,
      language: rest.isEmpty ? nil : rest
    )
  }

  private mutating func codeBlock(fence: Fence) -> MarkdownBlock {
    index += 1
    var body: [String] = []
    var isClosed = false

    while index < lines.count {
      let line = lines[index]
      if let closing = fenceInfo(line),
        closing.character == fence.character,
        closing.length >= fence.length,
        closing.language == nil
      {
        index += 1
        isClosed = true
        break
      }
      body.append(stripIndent(line, upTo: fence.indent))
      index += 1
    }

    return .code(
      language: fence.language,
      code: body.joined(separator: "\n"),
      isClosed: isClosed
    )
  }

  // MARK: Headings

  private func headingBlock(_ line: Substring) -> MarkdownBlock? {
    guard leadingIndent(line) < 4 else { return nil }
    let content = line.drop(while: { $0 == " " })
    let hashes = content.prefix(while: { $0 == "#" })
    guard (1...6).contains(hashes.count) else { return nil }
    let rest = content.dropFirst(hashes.count)
    guard rest.isEmpty || rest.first == " " else { return nil }

    var text = rest.trimmingCharacters(in: .whitespaces)
    // Closing sequences ("## Title ##") are decorative.
    while text.hasSuffix("#") {
      text = String(text.dropLast()).trimmingCharacters(in: .whitespaces)
    }
    return .heading(level: hashes.count, text: MarkdownInline.parse(text))
  }

  private func isThematicBreak(_ line: Substring) -> Bool {
    guard leadingIndent(line) < 4 else { return false }
    let content = line.filter { $0 != " " && $0 != "\t" }
    guard content.count >= 3 else { return false }
    guard let first = content.first, "-*_".contains(first) else { return false }
    return content.allSatisfy { $0 == first }
  }

  // MARK: Quotes

  private func isQuote(_ line: Substring) -> Bool {
    guard leadingIndent(line) < 4 else { return false }
    return line.drop(while: { $0 == " " }).first == ">"
  }

  private mutating func quoteBlock() -> MarkdownBlock {
    var paragraphs: [String] = []
    var current: [String] = []

    while index < lines.count, isQuote(lines[index]) {
      var content = lines[index].drop(while: { $0 == " " }).dropFirst()
      if content.first == " " { content = content.dropFirst() }
      let text = String(content)
      if text.trimmingCharacters(in: .whitespaces).isEmpty {
        if !current.isEmpty {
          paragraphs.append(current.joined(separator: "\n"))
          current = []
        }
      } else {
        current.append(text)
      }
      index += 1
    }
    if !current.isEmpty {
      paragraphs.append(current.joined(separator: "\n"))
    }

    return .quote(paragraphs.map(MarkdownInline.parse))
  }

  // MARK: Lists

  private struct ListMarker {
    let indent: Int
    let isOrdered: Bool
    let ordinal: Int
    /// Characters to drop from the trimmed line to reach the item's text.
    let markerLength: Int
  }

  private func listMarker(_ line: Substring) -> ListMarker? {
    let indent = leadingIndent(line)
    let content = line.drop(while: { $0 == " " || $0 == "\t" })
    guard let first = content.first else { return nil }

    if first == "-" || first == "*" || first == "+" {
      guard content.dropFirst().first == " " else { return nil }
      return ListMarker(
        indent: indent,
        isOrdered: false,
        ordinal: 0,
        markerLength: 1
      )
    }

    guard first.isNumber else { return nil }
    let digits = content.prefix(while: { $0.isNumber })
    guard digits.count <= 9 else { return nil }
    let afterDigits = content.dropFirst(digits.count)
    guard let delimiter = afterDigits.first, delimiter == "." || delimiter == ")"
    else { return nil }
    guard afterDigits.dropFirst().first == " " else { return nil }

    return ListMarker(
      indent: indent,
      isOrdered: true,
      ordinal: Int(digits) ?? 1,
      markerLength: digits.count + 1
    )
  }

  private mutating func listBlock() -> MarkdownBlock {
    guard let first = listMarker(lines[index]) else {
      return paragraphBlock()
    }

    let isOrdered = first.isOrdered
    /// Collected before depths are assigned: indentation conventions vary
    /// between two and four spaces, so nesting is derived from the set of
    /// indents actually used rather than from a fixed divisor.
    var raw: [(indent: Int, ordinal: Int, text: String)] = []

    while index < lines.count {
      let line = lines[index]
      guard let marker = listMarker(line), marker.isOrdered == isOrdered else {
        // A blank line only ends the list if the next line is not an item.
        if line.trimmingCharacters(in: .whitespaces).isEmpty,
          index + 1 < lines.count,
          let following = listMarker(lines[index + 1]),
          following.isOrdered == isOrdered
        {
          index += 1
          continue
        }
        break
      }

      let content = line.drop(while: { $0 == " " || $0 == "\t" })
      let body = content.dropFirst(marker.markerLength)
        .drop(while: { $0 == " " })
      raw.append(
        (indent: marker.indent, ordinal: marker.ordinal, text: String(body))
      )
      index += 1
    }

    let depths = Array(Set(raw.map(\.indent))).sorted()
    var depthByIndent: [Int: Int] = [:]
    for (offset, indent) in depths.enumerated() {
      depthByIndent[indent] = min(offset, Self.bullets.count - 1)
    }

    var items: [MarkdownListItem] = []
    /// Running ordinal per depth, so "1. 1. 1." still renders 1, 2, 3.
    var ordinalByDepth: [Int: Int] = [:]

    for entry in raw {
      let depth = depthByIndent[entry.indent] ?? 0
      for deeper in ordinalByDepth.keys where deeper > depth {
        ordinalByDepth[deeper] = nil
      }

      let renderedMarker: String
      if isOrdered {
        let ordinal = ordinalByDepth[depth].map { $0 + 1 } ?? entry.ordinal
        ordinalByDepth[depth] = ordinal
        renderedMarker = "\(ordinal)."
      } else {
        renderedMarker = Self.bullets[depth]
      }

      items.append(
        MarkdownListItem(
          depth: depth,
          marker: renderedMarker,
          text: MarkdownInline.parse(entry.text)
        )
      )
    }

    return .list(MarkdownList(isOrdered: isOrdered, items: items))
  }

  private static let bullets = ["•", "◦", "▪", "·"]

  // MARK: Tables

  private mutating func tableBlock() -> MarkdownBlock? {
    guard index + 1 < lines.count else { return nil }
    let headerLine = lines[index]
    guard headerLine.contains(where: { $0 == "|" }) else { return nil }
    guard let alignments = delimiterAlignments(lines[index + 1]) else {
      return nil
    }
    let header = splitRow(headerLine)
    guard header.count == alignments.count else { return nil }

    index += 2
    var rows: [[AttributedString]] = []
    while index < lines.count, lines[index].contains(where: { $0 == "|" }) {
      let cells = splitRow(lines[index])
      guard !cells.isEmpty else { break }
      var normalized = cells.map(MarkdownInline.parse)
      if normalized.count < alignments.count {
        normalized += Array(
          repeating: AttributedString(),
          count: alignments.count - normalized.count
        )
      }
      rows.append(Array(normalized.prefix(alignments.count)))
      index += 1
    }

    return .table(
      MarkdownTable(
        header: header.map(MarkdownInline.parse),
        rows: rows,
        alignments: alignments
      )
    )
  }

  private func delimiterAlignments(_ line: Substring) -> [MarkdownColumnAlignment]? {
    guard line.contains(where: { $0 == "|" }),
      line.contains(where: { $0 == "-" })
    else { return nil }
    let cells = splitRow(line)
    guard !cells.isEmpty else { return nil }

    var alignments: [MarkdownColumnAlignment] = []
    for cell in cells {
      let trimmed = cell.trimmingCharacters(in: .whitespaces)
      let core = trimmed.trimmingCharacters(in: CharacterSet(charactersIn: ":"))
      guard !core.isEmpty, core.allSatisfy({ $0 == "-" }) else { return nil }
      switch (trimmed.hasPrefix(":"), trimmed.hasSuffix(":")) {
      case (true, true): alignments.append(.center)
      case (false, true): alignments.append(.trailing)
      default: alignments.append(.leading)
      }
    }
    return alignments
  }

  private func splitRow(_ line: Substring) -> [String] {
    var trimmed = line.trimmingCharacters(in: .whitespaces)
    if trimmed.hasPrefix("|") { trimmed.removeFirst() }
    if trimmed.hasSuffix("|") { trimmed.removeLast() }
    return trimmed.components(separatedBy: "|")
      .map { $0.trimmingCharacters(in: .whitespaces) }
  }

  // MARK: Paragraphs

  private mutating func paragraphBlock() -> MarkdownBlock {
    var body: [String] = []

    while index < lines.count {
      let line = lines[index]
      if line.trimmingCharacters(in: .whitespaces).isEmpty { break }
      if !body.isEmpty {
        // A construct that can begin a block interrupts the paragraph.
        if fenceInfo(line) != nil
          || headingBlock(line) != nil
          || isThematicBreak(line)
          || isQuote(line)
          || listMarker(line) != nil
        {
          break
        }
      }
      body.append(String(line))
      index += 1
    }

    // Single newlines are meaningful in chat output, so soft breaks are kept
    // rather than folded into spaces.
    return .paragraph(MarkdownInline.parse(body.joined(separator: "\n")))
  }

  // MARK: Utilities

  private func leadingIndent(_ line: Substring) -> Int {
    var count = 0
    for character in line {
      if character == " " {
        count += 1
      } else if character == "\t" {
        count += 4
      } else {
        break
      }
    }
    return count
  }

  private func stripIndent(_ line: Substring, upTo limit: Int) -> String {
    var removed = 0
    var remainder = line
    while removed < limit, let first = remainder.first, first == " " {
      remainder = remainder.dropFirst()
      removed += 1
    }
    return String(remainder)
  }
}

// MARK: - Inline

enum MarkdownInline {
  /// Parses inline spans — emphasis, strong, inline code, links — leaving
  /// block structure to `MarkdownParser`. Whitespace is preserved so soft
  /// line breaks inside a paragraph survive.
  static func parse(_ text: String) -> AttributedString {
    guard !text.isEmpty else { return AttributedString() }
    do {
      return try AttributedString(
        markdown: text,
        options: AttributedString.MarkdownParsingOptions(
          allowsExtendedAttributes: true,
          interpretedSyntax: .inlineOnlyPreservingWhitespace,
          failurePolicy: .returnPartiallyParsedIfPossible
        )
      )
    } catch {
      // A half-written emphasis run mid-stream must still render as text.
      return AttributedString(text)
    }
  }
}

// MARK: - Incremental parsing

/// Caches parsed markdown per message so a streaming reply does not re-run the
/// inline parser over text that can no longer change (UX-016, UX-021).
///
/// Only the final block of a document is still open while a reply streams, so
/// every earlier block is reused verbatim and keeps its identity — which is
/// what stops already-rendered content from re-laying-out on each delta.
@MainActor
final class MarkdownCache {
  private struct Entry {
    let source: String
    let document: MarkdownDocument
  }

  private var entries: [UUID: Entry] = [:]
  /// Bounds memory when a long thread is scrolled; the transcript only ever
  /// renders a window of messages.
  private let capacity: Int

  init(capacity: Int = 64) {
    self.capacity = capacity
  }

  func document(for source: String, id: UUID) -> MarkdownDocument {
    if let entry = entries[id] {
      if entry.source == source {
        return entry.document
      }
      if source.count > entry.source.count,
        !entry.document.closedSource.isEmpty,
        source.hasPrefix(entry.document.closedSource)
      {
        let closed = entry.document.nodes.dropLast()
        let tail = String(source.dropFirst(entry.document.closedSource.count))
        var parser = MarkdownParser(source: tail)
        let parsed = parser.parse(idOffset: closed.count)
        let document = MarkdownDocument(
          nodes: Array(closed) + parsed.nodes,
          closedSource: entry.document.closedSource + parsed.closedSource
        )
        store(Entry(source: source, document: document), id: id)
        return document
      }
    }

    let document = MarkdownDocument.parse(source)
    store(Entry(source: source, document: document), id: id)
    return document
  }

  func forget(id: UUID) {
    entries[id] = nil
  }

  private func store(_ entry: Entry, id: UUID) {
    if entries[id] == nil, entries.count >= capacity {
      entries.removeAll()
    }
    entries[id] = entry
  }
}
