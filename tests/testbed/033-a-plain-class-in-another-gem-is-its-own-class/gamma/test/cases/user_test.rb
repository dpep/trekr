class UserTest
  def test_login
    user = User.new
    user.authenticate("secret")
    user.save
  end
end
