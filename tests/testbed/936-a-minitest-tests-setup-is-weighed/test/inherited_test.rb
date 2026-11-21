require "test_helper"

class InheritedTest < BaseCase
  def test_value
    assert_equal @n, Inherited.new.value
  end
end
