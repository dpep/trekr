class Base
  def run
    setup
    self.finish
    other
  end

  def setup
  end

  def other
  end
end

class Child < Base
  def setup
  end

  def finish
  end
end

class Stranger
  def setup
  end
end

class Sibling < Base
  def other
  end
end

Base.new.other
