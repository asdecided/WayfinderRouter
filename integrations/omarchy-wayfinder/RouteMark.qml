import QtQuick

Item {
  id: root

  property color markColor: "white"
  property color statusColor: markColor
  property bool reachable: false

  onMarkColorChanged: canvas.requestPaint()
  onStatusColorChanged: canvas.requestPaint()
  onReachableChanged: canvas.requestPaint()

  Canvas {
    id: canvas
    anchors.fill: parent
    antialiasing: true

    onPaint: {
      var context = getContext("2d")
      var scale = Math.min(width, height) / 20
      context.clearRect(0, 0, width, height)
      context.lineCap = "round"
      context.lineJoin = "round"
      context.strokeStyle = root.markColor
      context.fillStyle = root.markColor
      context.lineWidth = Math.max(1.25, 1.65 * scale)

      context.beginPath()
      context.moveTo(3 * scale, 10 * scale)
      context.lineTo(9 * scale, 10 * scale)
      context.lineTo(15.5 * scale, 4 * scale)
      context.moveTo(9 * scale, 10 * scale)
      context.lineTo(15.5 * scale, 16 * scale)
      context.stroke()

      var nodes = [[3, 10], [16, 4], [16, 16]]
      for (var i = 0; i < nodes.length; i++) {
        context.beginPath()
        context.arc(nodes[i][0] * scale, nodes[i][1] * scale, 1.9 * scale, 0, Math.PI * 2)
        context.fillStyle = i === 0 && root.reachable ? root.statusColor : root.markColor
        context.fill()
      }
    }
  }
}
