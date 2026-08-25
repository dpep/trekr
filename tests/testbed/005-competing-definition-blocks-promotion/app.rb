class Widget
  def ship
  end
end
class Job
  def ship
  end

  def run
    @widget.ship
  end
end
