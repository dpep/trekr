class Widget
  def persist
  end
end
class Job
  def run
    w = Widget.new
    w.persist
  end
end
