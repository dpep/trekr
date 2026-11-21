class Factory
  def self.build(n)
    raise ArgumentError, "negative" if n.negative?
    n
  end
end

class Reopened
  def value
    2
  end
end

Factory.build(1)
