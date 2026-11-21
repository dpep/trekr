require "test_helper"

class PlainTest < Minitest::Test
  def setup
    @n = 2
  end

  def test_value
    assert_equal @n, Plain.new.value
  end

  def teardown
    Factory.build(0)
  end
end
