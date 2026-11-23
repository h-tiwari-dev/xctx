import AppKit
import WorkspaceCore

/// A window that reports first-responder changes, so the focused pane can follow clicks and tabbing.
final class FocusWindow: NSWindow {
    var onResponder: (() -> Void)?
    override func makeFirstResponder(_ responder: NSResponder?) -> Bool {
        let ok = super.makeFirstResponder(responder)
        onResponder?()
        return ok
    }
}

/// A pane's frame: a header (title, split and close buttons) over its content.
@MainActor final class PaneContainer: NSView {
    let paneId: Int
    private(set) var content: PaneContent
    private let titleLabel = NSTextField(labelWithString: "")
    private let icon = NSImageView()
    private let body = NSView()
    var onAction: ((String) -> Void)?
    var focused = false { didSet { refreshRing() } }

    init(id: Int, content: PaneContent) {
        paneId = id
        self.content = content
        super.init(frame: .zero)
        wantsLayer = true
        layer?.cornerRadius = 8
        layer?.masksToBounds = true
        layer?.borderWidth = 1
        layer?.backgroundColor = Theme.bg.cgColor
        titleLabel.font = .systemFont(ofSize: 11, weight: .semibold)
        titleLabel.lineBreakMode = .byTruncatingTail
        titleLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        icon.contentTintColor = Theme.dim
        let header = NSStackView(views: [
            icon, titleLabel,
            symbolButton("rectangle.split.2x1", "Split right", self, #selector(splitRight)),
            symbolButton("rectangle.split.1x2", "Split down", self, #selector(splitDown)),
            symbolButton("xmark", "Close pane", self, #selector(closePane)),
        ])
        header.spacing = 4
        header.edgeInsets = NSEdgeInsets(top: 0, left: 8, bottom: 0, right: 4)
        header.wantsLayer = true
        header.layer?.backgroundColor = Theme.panel.cgColor
        for v in [header, body] as [NSView] {
            v.translatesAutoresizingMaskIntoConstraints = false
            addSubview(v)
        }
        NSLayoutConstraint.activate([
            header.topAnchor.constraint(equalTo: topAnchor),
            header.leadingAnchor.constraint(equalTo: leadingAnchor),
            header.trailingAnchor.constraint(equalTo: trailingAnchor),
            header.heightAnchor.constraint(equalToConstant: 24),
            body.topAnchor.constraint(equalTo: header.bottomAnchor),
            body.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 4),
            body.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -2),
            body.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -2),
        ])
        install()
    }

    required init?(coder: NSCoder) { fatalError() }

    private func install() {
        let v = content.view
        v.frame = body.bounds
        v.autoresizingMask = [.width, .height]
        body.addSubview(v)
        refreshTitle()
    }

    func setContent(_ c: PaneContent) {
        content.view.removeFromSuperview()
        content = c
        install()
    }

    func refreshTitle() {
        titleLabel.stringValue = content.title
        icon.image = NSImage(systemSymbolName: content.kind?.symbol ?? "plus.square.dashed", accessibilityDescription: nil)
        refreshRing()
    }

    private func refreshRing() {
        layer?.borderColor = (focused ? Theme.accent : Theme.ring).cgColor
        titleLabel.textColor = focused ? Theme.ink : Theme.dim
    }

    override func mouseDown(with event: NSEvent) { onAction?("focus") }
    @objc private func splitRight() { onAction?("right") }
    @objc private func splitDown() { onAction?("down") }
    @objc private func closePane() { onAction?("close") }
}

/// One split of the tree; reports its divider position back as a ratio.
final class PaneSplitView: NSSplitView, NSSplitViewDelegate {
    let splitId: Int
    var ratio: Double
    var onRatio: ((Int, Double) -> Void)?
    private var applied = false

    init(id: Int, sideBySide: Bool, ratio: Double) {
        splitId = id
        self.ratio = ratio
        super.init(frame: NSRect(x: 0, y: 0, width: 800, height: 600))
        isVertical = sideBySide
        dividerStyle = .thin
        delegate = self
    }

    required init?(coder: NSCoder) { fatalError() }

    override var dividerThickness: CGFloat { 6 }
    override var dividerColor: NSColor { .clear }

    private var length: CGFloat { (isVertical ? bounds.width : bounds.height) - dividerThickness }

    override func layout() {
        super.layout()
        if !applied, subviews.count == 2, length > 0 {
            applied = true
            setPosition(CGFloat(ratio) * length, ofDividerAt: 0)
        }
    }

    func splitViewDidResizeSubviews(_ notification: Notification) {
        guard applied, subviews.count == 2, length > 0 else { return }
        let a = isVertical ? subviews[0].frame.width : subviews[0].frame.height
        ratio = Double(a / length)
        onRatio?(splitId, ratio)
    }

    func splitView(_ splitView: NSSplitView, constrainMinCoordinate p: CGFloat, ofSubviewAt i: Int) -> CGFloat { 80 }
    func splitView(_ splitView: NSSplitView, constrainMaxCoordinate p: CGFloat, ofSubviewAt i: Int) -> CGFloat { length - 80 }
}

