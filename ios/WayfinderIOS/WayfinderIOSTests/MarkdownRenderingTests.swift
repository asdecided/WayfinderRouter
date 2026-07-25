import Foundation
import XCTest

@testable import WayfinderIOS

/// UX-006 and UX-016. Assistant output was plain `Text`; these cover the block
/// grammar that replaced it, and the streaming behaviour that keeps already
/// rendered content from re-laying-out.
final class MarkdownParsingTests: XCTestCase {

  // MARK: - Blocks

  func testParagraphsSplitOnBlankLines() {
    let document = MarkdownDocument.parse("First para.\n\nSecond para.")

    XCTAssertEqual(document.nodes.count, 2)
    XCTAssertEqual(plainText(document.nodes[0].block), "First para.")
    XCTAssertEqual(plainText(document.nodes[1].block), "Second para.")
  }

  func testSoftLineBreaksArePreservedInsideAParagraph() {
    // Chat output uses single newlines meaningfully; folding them into spaces
    // would silently reflow the reply.
    let document = MarkdownDocument.parse("Line one\nLine two")

    XCTAssertEqual(document.nodes.count, 1)
    XCTAssertEqual(plainText(document.nodes[0].block), "Line one\nLine two")
  }

  func testHeadingLevelsParse() {
    let document = MarkdownDocument.parse("# One\n\n### Three\n\n###### Six")

    let levels = document.nodes.compactMap { node -> Int? in
      if case .heading(let level, _) = node.block { return level }
      return nil
    }
    XCTAssertEqual(levels, [1, 3, 6])
  }

  func testSevenHashesIsNotAHeading() {
    let document = MarkdownDocument.parse("####### Not a heading")

    guard case .paragraph = document.nodes.first?.block else {
      return XCTFail("expected a paragraph, got \(String(describing: document.nodes.first))")
    }
  }

  func testHashWithoutSpaceIsNotAHeading() {
    let document = MarkdownDocument.parse("#hashtag")

    guard case .paragraph = document.nodes.first?.block else {
      return XCTFail("expected a paragraph")
    }
  }

  func testUnorderedListCollectsItems() {
    let document = MarkdownDocument.parse("- alpha\n- beta\n- gamma")

    guard case .list(let list) = document.nodes.first?.block else {
      return XCTFail("expected a list")
    }
    XCTAssertFalse(list.isOrdered)
    XCTAssertEqual(list.items.map { String($0.text.characters) }, ["alpha", "beta", "gamma"])
    XCTAssertEqual(Set(list.items.map(\.depth)), [0])
  }

  func testOrderedListRenumbersSequentially() {
    // Models often emit "1." for every item.
    let document = MarkdownDocument.parse("1. alpha\n1. beta\n1. gamma")

    guard case .list(let list) = document.nodes.first?.block else {
      return XCTFail("expected a list")
    }
    XCTAssertTrue(list.isOrdered)
    XCTAssertEqual(list.items.map(\.marker), ["1.", "2.", "3."])
  }

  func testOrderedListHonoursAnExplicitStart() {
    let document = MarkdownDocument.parse("3. three\n4. four")

    guard case .list(let list) = document.nodes.first?.block else {
      return XCTFail("expected a list")
    }
    XCTAssertEqual(list.items.map(\.marker), ["3.", "4."])
  }

  func testNestedListsNormaliseBothIndentConventions() {
    for indent in ["  ", "    "] {
      let document = MarkdownDocument.parse("- top\n\(indent)- nested\n- top again")

      guard case .list(let list) = document.nodes.first?.block else {
        return XCTFail("expected a list for indent \(indent.count)")
      }
      XCTAssertEqual(
        list.items.map(\.depth),
        [0, 1, 0],
        "indent width \(indent.count) did not normalise to one nesting level"
      )
    }
  }

  func testFencedCodeBlockKeepsContentVerbatim() {
    let source = "```swift\nlet x = 1\n\n  indented()\n```"
    let document = MarkdownDocument.parse(source)

    guard case .code(let language, let code, let isClosed) = document.nodes.first?.block
    else {
      return XCTFail("expected a code block")
    }
    XCTAssertEqual(language, "swift")
    XCTAssertEqual(code, "let x = 1\n\n  indented()")
    XCTAssertTrue(isClosed)
  }

  func testMarkdownInsideACodeBlockIsNotInterpreted() {
    let document = MarkdownDocument.parse("```\n# not a heading\n- not a list\n```")

    guard case .code(_, let code, _) = document.nodes.first?.block else {
      return XCTFail("expected a code block")
    }
    XCTAssertEqual(code, "# not a heading\n- not a list")
    XCTAssertEqual(document.nodes.count, 1)
  }

  func testUnterminatedFenceStillRendersAsCode() {
    // Every delta of a streaming code block looks like this.
    let document = MarkdownDocument.parse("```python\nprint(1)")

    guard case .code(let language, let code, let isClosed) = document.nodes.first?.block
    else {
      return XCTFail("expected a code block")
    }
    XCTAssertEqual(language, "python")
    XCTAssertEqual(code, "print(1)")
    XCTAssertFalse(isClosed)
  }

  func testTildeFenceIsSupportedAndBacktickDoesNotCloseIt() {
    let document = MarkdownDocument.parse("~~~\n```\n~~~")

    guard case .code(_, let code, let isClosed) = document.nodes.first?.block else {
      return XCTFail("expected a code block")
    }
    XCTAssertEqual(code, "```")
    XCTAssertTrue(isClosed)
  }

  func testBlockQuoteGroupsItsParagraphs() {
    let document = MarkdownDocument.parse("> first\n> still first\n>\n> second")

    guard case .quote(let paragraphs) = document.nodes.first?.block else {
      return XCTFail("expected a quote")
    }
    XCTAssertEqual(paragraphs.count, 2)
    XCTAssertEqual(String(paragraphs[0].characters), "first\nstill first")
    XCTAssertEqual(String(paragraphs[1].characters), "second")
  }

  func testThematicBreakParses() {
    let document = MarkdownDocument.parse("above\n\n---\n\nbelow")

    XCTAssertEqual(document.nodes.count, 3)
    guard case .rule = document.nodes[1].block else {
      return XCTFail("expected a rule")
    }
  }

  func testPipeTableParsesHeaderRowsAndAlignment() {
    let source = """
      | Name | Score | Note |
      |:-----|------:|:----:|
      | a | 1 | x |
      | b | 2 | y |
      """
    let document = MarkdownDocument.parse(source)

    guard case .table(let table) = document.nodes.first?.block else {
      return XCTFail("expected a table")
    }
    XCTAssertEqual(table.header.map { String($0.characters) }, ["Name", "Score", "Note"])
    XCTAssertEqual(table.alignments, [.leading, .trailing, .center])
    XCTAssertEqual(table.rows.count, 2)
    XCTAssertEqual(table.rows[1].map { String($0.characters) }, ["b", "2", "y"])
  }

  func testAParagraphContainingAPipeIsNotATable() {
    let document = MarkdownDocument.parse("use a | b in the shell\nand carry on")

    guard case .paragraph = document.nodes.first?.block else {
      return XCTFail("expected a paragraph")
    }
  }

  func testAListInterruptsAParagraph() {
    let document = MarkdownDocument.parse("Intro:\n- one\n- two")

    XCTAssertEqual(document.nodes.count, 2)
    guard case .paragraph = document.nodes[0].block,
      case .list = document.nodes[1].block
    else {
      return XCTFail("expected a paragraph then a list")
    }
  }

  // MARK: - Inline

  func testInlineEmphasisAndCodeCarryTheirIntents() {
    let document = MarkdownDocument.parse("plain **bold** and `code` here")

    guard case .paragraph(let text) = document.nodes.first?.block else {
      return XCTFail("expected a paragraph")
    }
    XCTAssertEqual(String(text.characters), "plain bold and code here")

    let intents = text.runs.compactMap(\.inlinePresentationIntent)
    XCTAssertTrue(intents.contains { $0.contains(.stronglyEmphasized) })
    XCTAssertTrue(intents.contains { $0.contains(.code) })
  }

  func testLinksSurviveAsTappableAttributes() {
    let document = MarkdownDocument.parse("see [the docs](https://example.com/a)")

    guard case .paragraph(let text) = document.nodes.first?.block else {
      return XCTFail("expected a paragraph")
    }
    let links = text.runs.compactMap(\.link)
    XCTAssertEqual(links, [URL(string: "https://example.com/a")])
    XCTAssertEqual(String(text.characters), "see the docs")
  }

  func testHalfWrittenEmphasisStillRendersItsText() {
    // Mid-stream the reply frequently ends inside a markup run.
    let document = MarkdownDocument.parse("an unfinished **bold")

    guard case .paragraph(let text) = document.nodes.first?.block else {
      return XCTFail("expected a paragraph")
    }
    XCTAssertTrue(String(text.characters).contains("unfinished"))
    XCTAssertTrue(String(text.characters).contains("bold"))
  }

  func testEmptySourceProducesNoBlocks() {
    XCTAssertTrue(MarkdownDocument.parse("").isEmpty)
    XCTAssertTrue(MarkdownDocument.parse("\n\n  \n").isEmpty)
  }

  // MARK: - Helpers

  private func plainText(_ block: MarkdownBlock?) -> String? {
    guard case .paragraph(let text) = block else { return nil }
    return String(text.characters)
  }
}

/// Incremental parsing is the mechanism behind "streaming does not reflow
/// already-rendered content". These tests hold it to producing exactly what a
/// full re-parse would, one delta at a time.
final class MarkdownStreamingTests: XCTestCase {

  private let reply = """
    Here is the plan.

    ## Steps

    1. Read the file
    2. Change it

    ```swift
    let value = 1
    ```

    > A closing note.

    Done.
    """

  func testIncrementalParseMatchesAFullParseAtEveryDelta() {
    var accumulated = ""
    var document: MarkdownDocument?

    for character in reply {
      accumulated.append(character)
      document = MarkdownDocument.parse(accumulated, reusing: document)
      XCTAssertEqual(
        document?.nodes,
        MarkdownDocument.parse(accumulated).nodes,
        "incremental parse diverged at \(accumulated.count) characters"
      )
    }
  }

  func testClosedBlocksKeepTheirIdentityWhileTextIsAppended() {
    // Identity stability is what stops SwiftUI rebuilding earlier blocks, and
    // therefore what stops the transcript re-laying-out during a stream.
    var accumulated = ""
    var document: MarkdownDocument?
    var previous: [MarkdownBlockNode] = []

    for character in reply {
      accumulated.append(character)
      document = MarkdownDocument.parse(accumulated, reusing: document)
      let nodes = document?.nodes ?? []

      for node in nodes.dropLast() where node.id < previous.count - 1 {
        XCTAssertEqual(
          node,
          previous[node.id],
          "block \(node.id) changed after it was closed"
        )
      }
      previous = nodes
    }
  }

  /// The case that proved "everything but the last block is settled" wrong.
  /// A list ends because the next line is not yet an item; one more character
  /// turns that line into an item and the list absorbs it. Reuse must not have
  /// already frozen the list as a separate block.
  func testABlockThatLaterAbsorbsTheFollowingLineIsNotReusedEarly() {
    var document: MarkdownDocument?
    for source in [
      "1. Read the file\n2",
      "1. Read the file\n2.",
      "1. Read the file\n2. ",
      "1. Read the file\n2. Change it",
    ] {
      document = MarkdownDocument.parse(source, reusing: document)
      XCTAssertEqual(
        document?.nodes,
        MarkdownDocument.parse(source).nodes,
        "incremental parse diverged at \(source.debugDescription)"
      )
    }

    guard case .list(let list) = document?.nodes.first?.block else {
      return XCTFail("expected one list")
    }
    XCTAssertEqual(document?.nodes.count, 1)
    XCTAssertEqual(list.items.map(\.marker), ["1.", "2."])
  }

  /// A list continues across a blank line, so it inspects a line beyond the
  /// one that ended it. That lookahead has to count as "not settled yet".
  func testAListThatContinuesAcrossABlankLineIsNotReusedEarly() {
    var document: MarkdownDocument?
    for source in ["- a\n\n-", "- a\n\n- ", "- a\n\n- b"] {
      document = MarkdownDocument.parse(source, reusing: document)
      XCTAssertEqual(
        document?.nodes,
        MarkdownDocument.parse(source).nodes,
        "incremental parse diverged at \(source.debugDescription)"
      )
    }
    XCTAssertEqual(document?.nodes.count, 1, "the list split into two blocks")
  }

  func testIncrementalParseMatchesAFullParseAcrossVariedStructures() {
    let documents = [
      "intro\n\n- a\n- b\n\n- c\n\ntail",
      "1. one\n   1. inner\n2. two\n\npara",
      "before\n\n| a | b |\n|---|--:|\n| 1 | 2 |\n\nafter",
      "> one\n> two\n\n> three\n\nend",
      "a\n\n```py\nx=1\n```\n\n```\ny\n```\n\nz",
      "a\n\n---\n\nb\n***\nc",
      "# H1\ntext\n## H2\n\n- l1\n- l2",
      "a\n\nb\n\n\n",
      "- a\n\n- b\n\ntext | pipe\n---\nmore",
    ]

    for source in documents {
      var accumulated = ""
      var document: MarkdownDocument?
      for character in source {
        accumulated.append(character)
        document = MarkdownDocument.parse(accumulated, reusing: document)
        XCTAssertEqual(
          document?.nodes,
          MarkdownDocument.parse(accumulated).nodes,
          "diverged in \(source.debugDescription) at \(accumulated.count) chars"
        )
      }
    }
  }

  func testReparsingUnchangedContentIsStable() {
    let first = MarkdownDocument.parse(reply)
    let second = MarkdownDocument.parse(reply, reusing: first)

    XCTAssertEqual(first.nodes, second.nodes)
  }

  func testContentThatIsNotAnAppendFallsBackToAFullParse() {
    // Retry-in-place replaces a message's content outright.
    let original = MarkdownDocument.parse("# Original\n\nbody")
    let replaced = MarkdownDocument.parse(
      "Completely different",
      reusing: original
    )

    XCTAssertEqual(
      replaced.nodes,
      MarkdownDocument.parse("Completely different").nodes
    )
  }

  func testStreamingACodeBlockDoesNotRestructureWhenTheFenceCloses() {
    let open = MarkdownDocument.parse("intro\n\n```swift\nlet a = 1")
    let closed = MarkdownDocument.parse(
      "intro\n\n```swift\nlet a = 1\n```",
      reusing: open
    )

    XCTAssertEqual(open.nodes.count, closed.nodes.count)
    XCTAssertEqual(open.nodes[0], closed.nodes[0])
    guard case .code(_, _, let wasClosed) = open.nodes[1].block,
      case .code(let language, let code, let isClosed) = closed.nodes[1].block
    else {
      return XCTFail("expected code blocks")
    }
    XCTAssertFalse(wasClosed)
    XCTAssertTrue(isClosed)
    XCTAssertEqual(language, "swift")
    XCTAssertEqual(code, "let a = 1")
  }
}
