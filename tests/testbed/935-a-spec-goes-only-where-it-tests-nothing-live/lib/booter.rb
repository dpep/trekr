class Registry
  def self.boot!
    true
  end
end

class Booter
  def first
    1
  end

  def second
    2
  end

  def third
    3
  end
end

Registry.boot!
