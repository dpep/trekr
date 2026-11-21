require "test_helper"

class BlockTest < ActiveSupport::TestCase
  setup do
    @n = Factory.build(2)
  end

  test "value" do
    assert_equal @n, AsBlock.new.value
  end
end
