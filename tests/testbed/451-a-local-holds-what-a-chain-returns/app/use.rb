class Use
  def run
    widgets = Widget.where(id: 1).order(:id)
    widgets.limit(3)
    found = Widget.where(id: 2)
    found.pluck(:id)
    loose = Widget.where(id: 3)
    3.times { loose = loose.where(id: 4) }
    loose.pluck(:id)
  end
end
