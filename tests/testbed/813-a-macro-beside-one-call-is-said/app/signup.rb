class Signup
  def self.before_action(*) = nil
  before_action :set_invite

  def create = set_invite

  private

  def set_invite = 1
end
Signup.new.create
