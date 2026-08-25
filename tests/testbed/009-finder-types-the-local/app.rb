class Widget
  def ship
  end
end
class Job
  def run
    w = Widget.find(1)
    w.ship
  end
end
