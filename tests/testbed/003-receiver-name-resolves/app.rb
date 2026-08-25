class Widget
  def ship_it
  end
end
class Job
  def initialize(widget)
    @widget = widget
  end

  def run
    @widget.ship_it
  end
end
