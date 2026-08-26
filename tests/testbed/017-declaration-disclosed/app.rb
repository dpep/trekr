class Widget
  attr_reader :size

  def resize
  end
end

class Job
  def run
    w = Widget.new
    w.size
    w.resize
  end
end
