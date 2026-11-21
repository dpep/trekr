require "minitest/autorun"

class BaseCase < Minitest::Test
  def setup
    @n = Factory.build(2)
  end
end
