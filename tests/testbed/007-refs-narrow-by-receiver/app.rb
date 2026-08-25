class Widget
  def save
  end
end
class Gadget
  def save
  end
end
class Job
  def run
    w = Widget.new
    w.save
    g = Gadget.new
    g.save
  end
end
