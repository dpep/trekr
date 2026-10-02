class Use
  def run
    Widget.where(id: 1).order(:id).pluck(:id)
    Widget.active.pluck(:id)
    widget = Widget.new
    widget.parts.find(1)
  end
end
