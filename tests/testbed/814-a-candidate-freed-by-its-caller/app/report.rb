class Report
  def build
    header
    footer
  end

  def header
    title
  end

  def footer
  end

  def title
  end

  def summary
    digest
  end

  def digest
  end

  def ping
    pong
  end

  def pong
    ping
  end

  def drive(thing)
    thing.start
  end
end

class Engine
  def start
  end
end

class Motor
  def start
  end
end
