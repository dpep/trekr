require "test_helper"

class ReopenedTest < ActionDispatch::IntegrationTest
  test "value" do
    assert_equal @n, Reopened.new.value
  end
end
