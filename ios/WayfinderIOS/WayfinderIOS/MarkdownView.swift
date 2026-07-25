import SwiftUI
import UIKit

/// Renders a parsed markdown document as transcript content (UX-006, UX-016).
///
/// Each block is a separate view keyed by its stable identity, so appending
/// text during streaming only invalidates the final block. Everything the
/// reader has already read keeps its layout.
struct MarkdownTextView: View {
  let document: MarkdownDocument
  /// Draws a trailing cursor on the final block while a reply is arriving.
  var isStreaming: Bool = false

  var body: some View {
    VStack(alignment: .leading, spacing: WayfinderSpacing.small) {
      ForEach(document.nodes) { node in
        MarkdownBlockView(
          block: node.block,
          showsCursor: isStreaming && node.id == document.nodes.last?.id
        )
      }
    }
    .frame(maxWidth: .infinity, alignment: .leading)
  }
}

// MARK: - Blocks

private struct MarkdownBlockView: View {
  let block: MarkdownBlock
  let showsCursor: Bool

  var body: some View {
    switch block {
    case .paragraph(let text):
      MarkdownParagraph(text: text, showsCursor: showsCursor)

    case .heading(let level, let text):
      MarkdownParagraph(text: text, showsCursor: showsCursor)
        .font(Self.headingFont(level: level))
        .padding(.top, level <= 2 ? WayfinderSpacing.xSmall : 0)
        .accessibilityAddTraits(.isHeader)

    case .list(let list):
      MarkdownListView(list: list, showsCursor: showsCursor)

    case .code(let language, let code, _):
      MarkdownCodeBlock(
        language: language,
        code: code,
        showsCursor: showsCursor
      )

    case .quote(let paragraphs):
      MarkdownQuote(paragraphs: paragraphs, showsCursor: showsCursor)

    case .rule:
      Divider()
        .padding(.vertical, WayfinderSpacing.xSmall)
        .accessibilityHidden(true)

    case .table(let table):
      MarkdownTableView(table: table)
    }
  }

  private static func headingFont(level: Int) -> Font {
    switch level {
    case 1: .title2.weight(.bold)
    case 2: .title3.weight(.semibold)
    case 3: .headline
    default: .subheadline.weight(.semibold)
    }
  }
}

// MARK: - Text

private struct MarkdownParagraph: View {
  let text: AttributedString
  let showsCursor: Bool

  var body: some View {
    Group {
      if showsCursor {
        Text(MarkdownStyling.styled(text)) + MarkdownStyling.cursor
      } else {
        Text(MarkdownStyling.styled(text))
      }
    }
    .textSelection(.enabled)
    .fixedSize(horizontal: false, vertical: true)
  }
}

enum MarkdownStyling {
  /// A quiet trailing cursor. It is static rather than blinking: a pulse here
  /// would be motion with no state behind it, and would need a Reduce Motion
  /// path for no benefit.
  static var cursor: Text {
    Text(verbatim: "\u{258D}")
      .foregroundColor(WayfinderTheme.accent)
  }

  /// Applies presentation to the spans the inline parser marked up. Only
  /// inline code needs it; emphasis, strong, and links are rendered by
  /// SwiftUI from their intents.
  static func styled(_ text: AttributedString) -> AttributedString {
    var result = text
    let codeRanges = result.runs.compactMap { run in
      (run.inlinePresentationIntent?.contains(.code) ?? false) ? run.range : nil
    }
    for range in codeRanges {
      result[range].font = .system(.body, design: .monospaced)
      result[range].backgroundColor = Color.primary.opacity(0.07)
    }
    return result
  }
}

// MARK: - Lists

private struct MarkdownListView: View {
  let list: MarkdownList
  let showsCursor: Bool

  var body: some View {
    VStack(alignment: .leading, spacing: WayfinderSpacing.hairline) {
      ForEach(Array(list.items.enumerated()), id: \.offset) { offset, item in
        HStack(alignment: .firstTextBaseline, spacing: WayfinderSpacing.xSmall) {
          Text(item.marker)
            .foregroundStyle(.secondary)
            .monospacedDigit()
          MarkdownParagraph(
            text: item.text,
            showsCursor: showsCursor && offset == list.items.count - 1
          )
        }
        .padding(.leading, CGFloat(item.depth) * WayfinderSpacing.medium)
        .accessibilityElement(children: .combine)
      }
    }
  }
}

// MARK: - Quotes

private struct MarkdownQuote: View {
  let paragraphs: [AttributedString]
  let showsCursor: Bool

  var body: some View {
    HStack(alignment: .top, spacing: WayfinderSpacing.small) {
      Rectangle()
        .fill(.tertiary)
        .frame(width: 3)
        .accessibilityHidden(true)

      VStack(alignment: .leading, spacing: WayfinderSpacing.xSmall) {
        ForEach(Array(paragraphs.enumerated()), id: \.offset) { offset, text in
          MarkdownParagraph(
            text: text,
            showsCursor: showsCursor && offset == paragraphs.count - 1
          )
        }
      }
      .foregroundStyle(.secondary)
    }
    .fixedSize(horizontal: false, vertical: true)
  }
}

// MARK: - Code

private struct MarkdownCodeBlock: View {
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  @ScaledMetric(relativeTo: .footnote) private var controlSize: CGFloat = 28

  let language: String?
  let code: String
  let showsCursor: Bool

  @State private var didCopy = false

  var body: some View {
    VStack(alignment: .leading, spacing: 0) {
      header

      ScrollView(.horizontal, showsIndicators: false) {
        Group {
          if showsCursor {
            Text(code) + MarkdownStyling.cursor
          } else {
            Text(code)
          }
        }
        .font(.system(.callout, design: .monospaced))
        .textSelection(.enabled)
        .padding(.horizontal, WayfinderSpacing.small)
        .padding(.bottom, WayfinderSpacing.small)
        .padding(.top, language == nil ? WayfinderSpacing.small : 0)
      }
    }
    .frame(maxWidth: .infinity, alignment: .leading)
    .background(
      WayfinderTheme.raisedSurface,
      in: RoundedRectangle(cornerRadius: WayfinderRadius.small)
    )
    .overlay {
      RoundedRectangle(cornerRadius: WayfinderRadius.small)
        .stroke(WayfinderTheme.hairline, lineWidth: 1)
    }
    .accessibilityElement(children: .contain)
    .accessibilityLabel(
      language.map { "Code block, \($0)" } ?? "Code block"
    )
  }

  @ViewBuilder
  private var header: some View {
    HStack(spacing: WayfinderSpacing.xSmall) {
      if let language {
        Text(language)
          .font(.caption2.weight(.medium))
          .foregroundStyle(.secondary)
          .textCase(.lowercase)
      }

      Spacer(minLength: 0)

      Button {
        UIPasteboard.general.string = code
        didCopy = true
      } label: {
        Label(
          didCopy ? "Copied" : "Copy",
          systemImage: didCopy ? "checkmark" : "doc.on.doc"
        )
        .font(.caption2.weight(.medium))
        .labelStyle(.titleAndIcon)
        .frame(minHeight: controlSize)
        .contentTransition(.symbolEffect(.replace))
      }
      .buttonStyle(.plain)
      .foregroundStyle(didCopy ? WayfinderTheme.accent : Color.secondary)
      .accessibilityLabel(didCopy ? "Code copied" : "Copy code")
      .sensoryFeedback(.success, trigger: didCopy) { _, copied in copied }
      .wayfinderAnimation(
        WayfinderMotion.control,
        value: didCopy,
        reduceMotion: reduceMotion
      )
      .task(id: didCopy) {
        guard didCopy else { return }
        try? await Task.sleep(for: .seconds(2))
        didCopy = false
      }
    }
    .padding(.horizontal, WayfinderSpacing.small)
    .padding(.top, WayfinderSpacing.xSmall)
  }
}

// MARK: - Tables

private struct MarkdownTableView: View {
  let table: MarkdownTable

  var body: some View {
    Grid(
      alignment: .topLeading,
      horizontalSpacing: WayfinderSpacing.medium,
      verticalSpacing: WayfinderSpacing.xSmall
    ) {
      GridRow {
        ForEach(Array(table.header.enumerated()), id: \.offset) { offset, cell in
          // Column alignment is declared once, on the header cell.
          Text(MarkdownStyling.styled(cell))
            .font(.footnote.weight(.semibold))
            .gridColumnAlignment(alignment(offset))
        }
      }

      Divider()
        .gridCellUnsizedAxes(.horizontal)
        .gridCellColumns(max(1, table.header.count))

      ForEach(Array(table.rows.enumerated()), id: \.offset) { _, row in
        GridRow {
          ForEach(Array(row.enumerated()), id: \.offset) { _, cell in
            Text(MarkdownStyling.styled(cell))
              .font(.footnote)
          }
        }
      }
    }
    .padding(WayfinderSpacing.small)
    .frame(maxWidth: .infinity, alignment: .leading)
    .background(
      WayfinderTheme.raisedSurface,
      in: RoundedRectangle(cornerRadius: WayfinderRadius.small)
    )
    .accessibilityElement(children: .contain)
    .accessibilityLabel("Table")
  }

  private func alignment(_ column: Int) -> HorizontalAlignment {
    guard column < table.alignments.count else { return .leading }
    switch table.alignments[column] {
    case .leading: return .leading
    case .center: return .center
    case .trailing: return .trailing
    }
  }
}
