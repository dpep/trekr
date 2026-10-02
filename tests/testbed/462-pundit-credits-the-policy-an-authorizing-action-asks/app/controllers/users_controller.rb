class UsersController < ApplicationController
  def leave
    org = Organization.find_by(id: 1)
    authorize org
  end
end
