require "test_helper"

class OwnTest < Minitest::Test
  def setup
    @n = Factory.build(2)
  end

  def test_value
    assert_equal @n, Own.new.value
  end
end
