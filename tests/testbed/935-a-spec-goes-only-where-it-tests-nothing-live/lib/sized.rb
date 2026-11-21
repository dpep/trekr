class Pricing
  def self.doubled(n)
    n * 2
  end
end

class Sized
  def initialize(size)
    @size = size
  end

  def total
    @size
  end
end

Pricing.doubled(1)
