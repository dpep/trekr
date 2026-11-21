require "test_helper"

class MissingTest < ActiveSupport::TestCase
  setup :prepare

  test "value" do
    assert_equal 2, Missing.new.value
  end
end
