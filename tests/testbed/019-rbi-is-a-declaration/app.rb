class Job
  def run
    w = Widget.new
    w.size
    w.weigh
  end
end

class Widget
  def weigh
  end
end
