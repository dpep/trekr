class Registry
  def initialize(name)
    raise ArgumentError, "blank" if name.empty?
    @name = name
  end
end

class Counter
  def initialize
    @n = 0
  end

  def bump!
    @n += 1
  end

  def count
    @n * 10
  end
end

module PriceHelper
  def format_price(cents)
    "$#{cents}"
  end
end

module Formats
  MONEY = ->(cents) { format("%.2f", cents) }
end

class Checkout
  include PriceHelper

  def total
    Registry.new("x")
    Counter.new.count
    Formats::MONEY.call(1)
    format_price(3)
  end
end

Checkout.new.total
