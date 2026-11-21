class Factory
  def self.build(n)
    raise ArgumentError, "negative" if n.negative?
    n
  end
end

Factory.build(1)
