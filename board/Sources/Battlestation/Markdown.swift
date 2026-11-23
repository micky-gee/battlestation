import SwiftUI

/// Agent-written markdown for the Deck. SwiftUI's `Text` renders inline
/// markdown (emphasis, code, links) but drops block structure — headings,
/// lists, code fences and tables run together — so this splits the text
/// into blocks itself and hands each block's inline runs to `Text`. It is a
/// reader for what agents actually send, not a CommonMark implementation:
/// anything it doesn't recognise falls through as a paragraph, so no input
/// is ever lost.
struct MarkdownView: View {
    let source: String
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(MarkdownBlock.parse(source).enumerated()), id: \.offset) { _, b in
                block(b)
            }
        }
        .textSelection(.enabled)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder private func block(_ b: MarkdownBlock) -> some View {
        switch b {
        case let .heading(level, text):
            inline(text)
                .font(level == 1 ? .title3.weight(.semibold) : level == 2 ? .headline : .subheadline.weight(.semibold))
                .padding(.top, 2)
        case let .paragraph(text):
            inline(text).font(.callout)
        case let .list(items):
            VStack(alignment: .leading, spacing: 3) {
                ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        Text(item.marker).font(.callout.monospacedDigit()).foregroundStyle(Theme.muted)
                        inline(item.text).font(.callout)
                    }
                    .padding(.leading, CGFloat(item.depth) * 16)
                }
            }
        case let .code(text):
            Text(text)
                .font(.system(.caption, design: .monospaced))
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(8)
                .background(RoundedRectangle(cornerRadius: 5).fill(Theme.bg))
                .overlay(RoundedRectangle(cornerRadius: 5).stroke(Theme.line))
        case let .quote(text):
            inline(text).font(.callout).foregroundStyle(Theme.muted)
                .padding(.leading, 10)
                .overlay(alignment: .leading) { Rectangle().fill(Theme.line).frame(width: 3) }
        case let .table(header, rows):
            Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 4) {
                GridRow { ForEach(Array(header.enumerated()), id: \.offset) { _, c in inline(c).font(.callout.weight(.semibold)) } }
                Divider()
                ForEach(Array(rows.enumerated()), id: \.offset) { _, r in
                    GridRow { ForEach(Array(r.enumerated()), id: \.offset) { _, c in inline(c).font(.callout) } }
                }
            }
        case .rule:
            Divider()
        }
    }

    private func inline(_ s: String) -> Text {
        let opts = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        let attr = (try? AttributedString(markdown: s, options: opts)) ?? AttributedString(s)
        return Text(attr)
    }
}

enum MarkdownBlock: Equatable {
    struct Item: Equatable { var depth: Int; var marker: String; var text: String }
    case heading(Int, String)
    case paragraph(String)
    case list([Item])
    case code(String)
    case quote(String)
    case table([String], [[String]])
    case rule

    /// The same text with the markup gone, one line per block — for
    /// surfaces that only show plain text (notification banners).
    static func plain(_ src: String) -> String {
        let opts = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        func strip(_ s: String) -> String {
            ((try? AttributedString(markdown: s, options: opts)).map { String($0.characters) }) ?? s
        }
        return parse(src).compactMap { b -> String? in
            switch b {
            case let .heading(_, t), let .paragraph(t), let .quote(t): return strip(t)
            case let .list(items): return items.map { "\($0.marker) \(strip($0.text))" }.joined(separator: "\n")
            case let .code(t): return t
            case let .table(h, rows): return ([h] + rows).map { $0.map(strip).joined(separator: " · ") }.joined(separator: "\n")
            case .rule: return nil
            }
        }.joined(separator: "\n")
    }

    static func parse(_ src: String) -> [MarkdownBlock] {
        let lines = src.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: "\n")
        var out: [MarkdownBlock] = []
        var para: [String] = []
        var i = 0
        func flush() {
            if !para.isEmpty { out.append(.paragraph(para.joined(separator: " "))); para = [] }
        }
        while i < lines.count {
            let line = lines[i]
            let t = line.trimmingCharacters(in: .whitespaces)
            if t.isEmpty { flush(); i += 1; continue }
            // Fenced code: everything to the closing fence, verbatim.
            if t.hasPrefix("```") || t.hasPrefix("~~~") {
                flush()
                let fence = String(t.prefix(3))
                var code: [String] = []
                i += 1
                while i < lines.count, !lines[i].trimmingCharacters(in: .whitespaces).hasPrefix(fence) {
                    code.append(lines[i]); i += 1
                }
                out.append(.code(code.joined(separator: "\n")))
                i += 1
                continue
            }
            if let (level, text) = heading(t) { flush(); out.append(.heading(level, text)); i += 1; continue }
            if isRule(t) { flush(); out.append(.rule); i += 1; continue }
            if t.hasPrefix(">") {
                flush()
                var q: [String] = []
                while i < lines.count, case let s = lines[i].trimmingCharacters(in: .whitespaces), s.hasPrefix(">") {
                    q.append(String(s.dropFirst()).trimmingCharacters(in: .whitespaces)); i += 1
                }
                out.append(.quote(q.joined(separator: " ")))
                continue
            }
            // Table: a pipe row followed by a |---| separator row.
            if t.hasPrefix("|"), i + 1 < lines.count, isTableSeparator(lines[i + 1]) {
                flush()
                let header = cells(t)
                var rows: [[String]] = []
                i += 2
                while i < lines.count, case let s = lines[i].trimmingCharacters(in: .whitespaces), s.hasPrefix("|") {
                    var r = cells(s)
                    r += Array(repeating: "", count: max(0, header.count - r.count))
                    rows.append(Array(r.prefix(header.count))); i += 1
                }
                out.append(.table(header, rows))
                continue
            }
            if listItem(line) != nil {
                flush()
                var items: [Item] = []
                while i < lines.count {
                    if let it = listItem(lines[i]) {
                        items.append(it)
                    } else if case let s = lines[i].trimmingCharacters(in: .whitespaces), !s.isEmpty,
                              lines[i].hasPrefix(" ") || lines[i].hasPrefix("\t"), !items.isEmpty {
                        items[items.count - 1].text += " " + s // continuation line
                    } else { break }
                    i += 1
                }
                out.append(.list(items))
                continue
            }
            para.append(t)
            i += 1
        }
        flush()
        return out
    }

    private static func heading(_ t: String) -> (Int, String)? {
        let hashes = t.prefix(while: { $0 == "#" }).count
        guard (1...6).contains(hashes), t.dropFirst(hashes).first == " " else { return nil }
        return (hashes, t.dropFirst(hashes).trimmingCharacters(in: .whitespaces))
    }

    private static func isRule(_ t: String) -> Bool {
        let s = t.replacingOccurrences(of: " ", with: "")
        guard s.count >= 3, let c = s.first, "-*_".contains(c) else { return false }
        return s.allSatisfy { $0 == c }
    }

    private static func isTableSeparator(_ l: String) -> Bool {
        let t = l.trimmingCharacters(in: .whitespaces)
        return t.contains("-") && t.allSatisfy { "|-: ".contains($0) }
    }

    private static func cells(_ row: String) -> [String] {
        var t = row.trimmingCharacters(in: .whitespaces)
        if t.hasPrefix("|") { t.removeFirst() }
        if t.hasSuffix("|") { t.removeLast() }
        return t.components(separatedBy: "|").map { $0.trimmingCharacters(in: .whitespaces) }
    }

    /// `- x`, `* x`, `+ x`, `1. x`, `1) x`, `- [ ] x`; depth from indent.
    private static func listItem(_ line: String) -> Item? {
        let indent = line.prefix(while: { $0 == " " || $0 == "\t" })
            .reduce(0) { $0 + ($1 == "\t" ? 4 : 1) }
        let t = line.trimmingCharacters(in: .whitespaces)
        var marker: String
        var rest: Substring
        if let f = t.first, "-*+".contains(f), t.dropFirst().first == " " {
            marker = "•"; rest = t.dropFirst(2)
        } else {
            let digits = t.prefix(while: \.isNumber)
            guard !digits.isEmpty, digits.count <= 3 else { return nil }
            let after = t.dropFirst(digits.count)
            guard let p = after.first, p == "." || p == ")", after.dropFirst().first == " " else { return nil }
            marker = digits + "."; rest = after.dropFirst(2)
        }
        if rest.hasPrefix("[ ] ") { marker = "☐"; rest = rest.dropFirst(4) }
        else if rest.hasPrefix("[x] ") || rest.hasPrefix("[X] ") { marker = "☑"; rest = rest.dropFirst(4) }
        return Item(depth: indent / 2, marker: marker, text: String(rest))
    }
}
