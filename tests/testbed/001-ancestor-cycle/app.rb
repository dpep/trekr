class Alpha < Beta::Inner
end
class Beta < Alpha::Other
  def ping
  end
end
class Job
  def run
    helper
  end
  def helper
  end
end
