class Job
  def run
    w = Widget.new("a")
    w.save
    Widget.build("b").save
  end
end
