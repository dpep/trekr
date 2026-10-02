class WidgetsController < ApplicationController
  def index
    authorize Widget
  end
end
