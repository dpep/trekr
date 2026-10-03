class WidgetsController < ApplicationController
  before_action :load_widget, only: [:show]

  def show
  end

  def create
    @gadget = Gadget.new
    render :show
  end

  def index
    @widget = Gadget.new
  end

  private

  def load_widget
    @widget = Widget.find(1)
  end
end
