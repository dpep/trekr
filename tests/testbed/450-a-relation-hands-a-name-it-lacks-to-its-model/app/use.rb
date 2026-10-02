class Use
  def run
    Widget.where(id: 1).popular
    Widget.visible.popular
    widget = Widget.new
    widget.parts.recent
    widget.spares.where(id: 1).popular
    rel = Widget.where(id: 2)
    rel.recent
  end
end
