class Job
  def run
    w = Widget.new("a")
    w.save
    w.zap
    Widget.build("b").save
  end
end
