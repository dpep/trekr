class ReportsController < ApplicationController
  def show
    @report = 1
  end

  def destroy
    authorize @report
  end
end
